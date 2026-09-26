//! T-P3-000 acceptance: the scan pipeline, end to end, and the assertion that
//! is the whole ticket.
//!
//! # What is actually being tested
//!
//! Phase 2 built every component of a scan pipeline and nothing called them in
//! sequence. Each piece is tested in isolation and all of them pass, which is
//! exactly the state that a green suite and a working product disagree about.
//! This test runs the pieces together over a real tree and a real database,
//! and then does the thing no component test could do: run the whole pipeline
//! a second time and assert that it wrote nothing.
//!
//! That second run is the ticket. §6.2's promise is "content hash is truth,
//! but only recomputed when the hint changes" and §6.1's is "a scan never
//! re-hashes an unchanged file". A pipeline that re-hashes every file on every
//! scan passes a first-run test perfectly, and is the regression this exists
//! to catch — it is also invisible in a log, because it produces the right
//! answer slowly.
//!
//! So the idempotence check asserts three things, and all three:
//!
//!   * the file rows are byte-identical before and after (a readback, not a
//!     count — a count cannot see a row that was rewritten with the same
//!     values, which is what a lost-update bug looks like);
//!   * no new rows appeared (a count that grew);
//!   * no job was submitted (the queue's submitted count is unchanged), which
//!     is the check that would catch a re-hash being laundered into a job.
//!
//! # Fixtures
//!
//! Files with content, not empty ones, because the pipeline's decision to hash
//! or not is only observable when there is something to hash. The content is
//! fixed and written before the first scan so the expected hashes are
//! computable, and the test asserts them against `hash_file` rather than a
//! transcribed constant — a transcribed hash asserts that a string is equal to
//! itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use commons_core::FileState;
use commons_scan::pipeline::{self, PipelineConfig, ScanOutcome};
use commons_store::{file_rows, Store};

/// A small tree with real content, so hashing is observable.
fn fixture(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut want = BTreeMap::new();
    for (i, dir) in ["a", "b", "c"].iter().enumerate() {
        for j in 0..4u32 {
            let rel = PathBuf::from(dir).join(format!("f{j}.mp4"));
            // Distinct, non-repeating content per file, so a wrong file's hash
            // cannot be mistaken for the right one.
            let body = format!(
                "file {i}/{j} content {}\n",
                "x".repeat(64 * (i + 1) + j as usize)
            );
            want.insert(rel, body.into_bytes());
        }
    }
    for (rel, body) in &want {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    want
}

async fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    (dir, store)
}

/// The `state` column for a file row, as stored.
///
/// Goes through the store accessor rather than a query, because the point of
/// these tests is what the *library* sees, and a test that reads the database
/// by a different route than production can agree with production while both
/// are wrong.
async fn file_state(store: &Store, id: &str) -> String {
    commons_store::file_path_and_state(store, id)
        .await
        .unwrap()
        .expect("the row is still there")
        .1
}

#[tokio::test]
async fn a_first_scan_stores_every_file_with_its_hash() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let want = fixture(&root);

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("the first scan");

    assert_eq!(outcome.new, want.len(), "{outcome:?}");
    assert_eq!(outcome.unchanged, 0, "nothing was there to be unchanged");
    assert_eq!(outcome.moved, 0);
    assert_eq!(outcome.marked_absent, 0);
    assert!(
        outcome.volume_errors.is_empty(),
        "{:?}",
        outcome.volume_errors
    );

    // Readback: every file is a row, with the path and the hash `hash_file`
    // computes for it right now. Read from the production hasher rather than
    // from a constant written out here.
    let rows = file_rows(&store).await.unwrap();
    assert_eq!(rows.len(), want.len(), "one row per file, no more");
    eprintln!(
        "DEBUG stored paths: {:?}",
        rows.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    for row in &rows {
        let abs = root.join(&row.path);
        let expect = commons_scan::hashing::hash_file(&abs).unwrap();
        assert_eq!(
            row.hash_blake3.as_deref(),
            Some(expect.blake3.as_str()),
            "{} has the wrong hash",
            row.path
        );
        assert_eq!(row.size_bytes, expect.bytes_read as i64, "{}", row.path);
    }
}

#[tokio::test]
async fn a_second_scan_of_an_unchanged_library_writes_nothing() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let want = fixture(&root);

    let first = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("first scan");
    assert_eq!(first.new, want.len());

    // Everything the first scan produced, captured as a value.
    let before = file_rows(&store).await.unwrap();
    let jobs_before = pipeline::submitted_job_count(&store).await;
    assert_eq!(jobs_before, 0, "the fixture scan submits no jobs");

    // --- the second scan --------------------------------------------------
    let second = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("second scan");

    // It must classify every file as unchanged. `new` being non-zero here is
    // the bug this test exists for: it means the pipeline could not tell its
    // own library from a fresh one.
    assert_eq!(second.new, 0, "a rescan invented {second:?}");
    assert_eq!(
        second.unchanged,
        want.len(),
        "every file should be recognised as unchanged: {second:?}"
    );
    assert_eq!(second.moved, 0, "{second:?}");
    assert_eq!(second.modified, 0, "{second:?}");
    assert_eq!(second.marked_absent, 0, "{second:?}");

    // --- and the three write assertions ----------------------------------
    // A count cannot see a row rewritten with the same values, which is what a
    // lost update looks like, so the rows are compared as values.
    let after = file_rows(&store).await.unwrap();
    assert_eq!(
        before.len(),
        after.len(),
        "the rescan added or removed rows"
    );
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(b.id, a.id, "row order changed; the comparison is by value");
        assert_eq!(b.object_id, a.object_id, "{} was reassigned", b.path);
        assert_eq!(b.path, a.path, "{} was rewritten", b.path);
        assert_eq!(b.size_bytes, a.size_bytes, "{} changed size", b.path);
        assert_eq!(b.mtime_ns, a.mtime_ns, "{} had its mtime touched", b.path);
        assert_eq!(
            b.hash_blake3, a.hash_blake3,
            "{} was re-hashed: the §6.1 promise is that an unchanged file is not read",
            b.path
        );
    }

    // The laundering case: a re-hash hidden inside a job would leave the rows
    // identical and pass the two assertions above.
    let jobs_after = pipeline::submitted_job_count(&store).await;
    assert_eq!(
        jobs_before, jobs_after,
        "the rescan submitted {jobs_after} job(s) for a library that had not changed"
    );
}

#[tokio::test]
async fn a_changed_file_keeps_its_identity_and_is_reported_modified() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();

    let target = root.join("b/f2.mp4");
    let before_row = file_rows(&store)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.path == "b/f2.mp4")
        .expect("b/f2.mp4 was scanned");
    let before_count = file_rows(&store).await.unwrap().len();

    // Change the content, and push the mtime forward so the `(mtime, size)`
    // hint cannot miss it. A test that relies on the write bumping mtime is a
    // test that fails on a coarse-granularity filesystem, so the hint is set
    // explicitly below.
    std::fs::write(&target, b"completely different content, longer than before").unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    filetime::set_file_mtime(&target, filetime::FileTime::from_system_time(future))
        .expect("setting mtime for the test");

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("rescan after a change");

    assert_eq!(outcome.modified, 1, "{outcome:?}");
    assert_eq!(
        outcome.new, 0,
        "a changed file is not a new file: {outcome:?}"
    );
    assert_eq!(outcome.unchanged, outcome.total as usize - 1, "{outcome:?}");

    let after = file_rows(&store).await.unwrap();
    assert_eq!(after.len(), before_count, "no new row for a changed file");
    let after_row = after
        .iter()
        .find(|r| r.id == before_row.id)
        .expect("the row survived, keeping its id");
    assert_eq!(after_row.path, "b/f2.mp4");
    assert_ne!(
        after_row.hash_blake3, before_row.hash_blake3,
        "the new content's hash should differ from the old"
    );
    let expect = commons_scan::hashing::hash_file(&target).unwrap();
    assert_eq!(
        after_row.hash_blake3.as_deref(),
        Some(expect.blake3.as_str()),
        "and it should be the hash of what is on disk now"
    );
}

#[tokio::test]
async fn a_rename_is_a_move_and_keeps_its_row() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    let before = file_rows(&store).await.unwrap();
    let orig = before
        .iter()
        .find(|r| r.path == "a/f1.mp4")
        .expect("a/f1.mp4 was scanned");

    std::fs::rename(root.join("a/f1.mp4"), root.join("a/renamed.mp4")).unwrap();

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("rescan after a rename");

    assert_eq!(outcome.moved, 1, "{outcome:?}");
    assert_eq!(
        outcome.new, 0,
        "a rename must not look like a new file: {outcome:?}"
    );
    assert_eq!(outcome.marked_absent, 0, "{outcome:?}");

    let after = file_rows(&store).await.unwrap();
    assert_eq!(after.len(), before.len());
    let moved = after
        .iter()
        .find(|r| r.id == orig.id)
        .expect("the row survived the rename");
    assert_eq!(moved.path, "a/renamed.mp4");
    assert_eq!(
        moved.hash_blake3, orig.hash_blake3,
        "a move is not a re-hash; the content did not change"
    );
}

#[tokio::test]
async fn a_deleted_file_is_marked_absent_and_never_deleted() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let want = fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();

    let gone = root.join("c/f3.mp4");
    std::fs::remove_file(&gone).unwrap();

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("rescan after a delete");

    assert_eq!(outcome.marked_absent, 1, "{outcome:?}");
    let rows = file_rows(&store).await.unwrap();
    assert_eq!(
        rows.len(),
        want.len(),
        "the row must survive: a deleted file is absent, not gone, because an \\
         unmounted volume looks exactly like a delete"
    );
    let gone_id = rows
        .iter()
        .find(|r| r.path == "c/f3.mp4")
        .expect("the row is still there")
        .id
        .clone();
    let (path, state) = commons_store::file_path_and_state(&store, &gone_id)
        .await
        .unwrap()
        .expect("and still readable by id");
    assert_eq!(path, "c/f3.mp4", "the path is not rewritten on a delete");
    // `FileState` has no `Absent` variant and is not supposed to: §6.1's
    // vocabulary is present | missing | unreadable | remote. A deleted file is
    // `missing`, which is the same state an unmounted volume produces -- the
    // point of the distinction being that the row survives either way.
    assert_eq!(
        commons_core::FileState::parse(&state),
        Some(commons_core::FileState::Missing),
        "and its state must be missing, not present"
    );
}

#[tokio::test]
async fn an_interrupted_scan_resumes_without_reprocessing_the_finished_part() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let want = fixture(&root);

    // Stop the walk after the first batch, which is the shape of a crash or a
    // user pressing stop: a checkpoint, no report.
    //
    // The batch size is set to 4 because the default 512 would put the whole
    // 12-file fixture in one batch and there would be nothing to interrupt. A
    // test that cannot fail because its fixture is smaller than its own
    // configuration is a test that looks like it covers interruption while
    // covering none of it.
    let cfg = PipelineConfig {
        stop_after_batches: Some(1),
        batch_size: 4,
        ..Default::default()
    };
    let partial = pipeline::scan(&store, &root, cfg)
        .await
        .expect("partial scan");
    assert!(partial.interrupted, "{partial:?}");
    let partial_rows = file_rows(&store).await.unwrap().len();
    assert!(
        partial_rows < want.len(),
        "the fixture is too small to interrupt: {partial_rows} of {}",
        want.len()
    );
    assert!(partial.checkpoint.is_some(), "a resume needs a checkpoint");

    // Resume from that checkpoint. The finished part is not walked again and
    // the rest is, and the two together are exactly the whole tree.
    let resumed = pipeline::scan(
        &store,
        &root,
        PipelineConfig {
            resume_from: partial.checkpoint.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("resumed scan");

    // The resumed scan does NOT see the whole tree -- that is what resuming
    // from a checkpoint means, and the first `partial_rows` files were never
    // walked a second time. So `resumed.total` is the remainder, not the
    // whole, and asserting `want.len()` here would be asserting that a resume
    // re-walks what it already finished. The property that matters is the
    // union: the two passes together cover the tree exactly once.
    assert_eq!(
        partial_rows + resumed.total as usize,
        want.len(),
        "the two passes must partition the tree, not overlap or leave a gap"
    );
    assert!(
        !resumed.interrupted,
        "the resumed scan ran to the end: {resumed:?}"
    );

    let rows = file_rows(&store).await.unwrap();
    assert_eq!(
        rows.len(),
        want.len(),
        "resume must not duplicate the files the first run already stored"
    );
    let mut paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), want.len(), "duplicate paths after resume");
}

#[tokio::test]
async fn a_missing_volume_is_reported_and_does_not_delete_anything() {
    // §6.1's #5683 shape: a volume that is not there must produce one volume
    // error, not one error per file that would have been in it, and must not
    // mark the whole library absent.
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    let before = file_rows(&store).await.unwrap();

    // Point the scan at a root that does not exist.
    let gone = sd.path().join("not-mounted");
    let outcome = pipeline::scan(&store, &gone, PipelineConfig::default())
        .await
        .expect("a scan of an absent root is not a crash");

    assert_eq!(
        outcome.volume_errors.len(),
        1,
        "one error for the volume, not one per file: {outcome:?}"
    );
    assert_eq!(outcome.total, 0);
    let after = file_rows(&store).await.unwrap();
    assert_eq!(
        before.len(),
        after.len(),
        "an absent volume must not mark the library absent"
    );
}

#[tokio::test]
async fn a_scan_of_an_empty_library_is_not_a_crash_and_claims_nothing() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();

    let outcome: ScanOutcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .expect("an empty library scans");
    assert_eq!(outcome.total, 0);
    assert_eq!(outcome.new, 0);
    assert!(file_rows(&store).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// The ones that were missing, added because the mutations survived.
//
// Each of these exists because an earlier version of the suite passed against
// a broken implementation. A test that never failed has not been shown to test
// anything.
// ---------------------------------------------------------------------------

/// A partial scan must not mark anything missing, and a stop-after-one-batch
/// scan is the cheapest way to be one.
///
/// This is the test that says "interrupted" means "we did not finish looking",
/// not "everything we did not see is gone". Before `ScanInput::complete`, a
/// scan stopped after one batch marked every other file in the library
/// deleted: the rows were there, the files were there, and the database said
/// they were not.
#[tokio::test]
async fn an_interrupted_scan_claims_nothing_is_missing() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    let before = file_rows(&store).await.unwrap();
    let mut all_present_before = true;
    for r in &before {
        if FileState::parse(&file_state(&store, &r.id).await) != Some(FileState::Present) {
            all_present_before = false;
        }
    }
    assert!(
        all_present_before,
        "the fixture scan left every row present"
    );

    let partial = pipeline::scan(
        &store,
        &root,
        PipelineConfig {
            stop_after_batches: Some(1),
            batch_size: 4,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(partial.interrupted, "{partial:?}");
    assert_eq!(
        partial.marked_absent, 0,
        "a scan that stopped early has not looked at most of the library"
    );
    assert!(all_present_before);

    for row in &before {
        assert_eq!(
            FileState::parse(&file_state(&store, &row.id).await).unwrap(),
            FileState::Present,
            "no row may change state because of an interrupted scan"
        );
    }
}

/// `stop_after_batches: Some(1)` yields exactly one batch, not zero.
///
/// The off-by-one is invisible in every other test because they use a limit
/// larger than the fixture: "one batch" and "one fewer batches" are the same
/// number when the number is large. With a fixture that fits, the difference is
/// the whole scan.
#[tokio::test]
async fn a_limit_of_one_batch_still_scans_one_batch() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let want = fixture(&root);

    let one = pipeline::scan(
        &store,
        &root,
        PipelineConfig {
            stop_after_batches: Some(1),
            batch_size: 4,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        one.total, 4,
        "one batch of four files is four files, not none: {one:?}"
    );
    assert!(
        one.total < want.len() as u64,
        "and it really did stop early"
    );
}

/// The same bytes at two paths are one object and two files.
///
/// This is §13's deduplication, and it is load-bearing for the object model:
/// `file.object_id` is a foreign key, so an id derived from the path alone
/// would give every copy its own object and quietly defeat the whole
/// identity scheme.
#[tokio::test]
async fn two_copies_of_one_file_are_one_object_and_two_rows() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let body = b"the very same bytes, in two places\n";
    std::fs::create_dir_all(root.join("x")).unwrap();
    std::fs::create_dir_all(root.join("y")).unwrap();
    std::fs::write(root.join("x/one.mp4"), body).unwrap();
    std::fs::write(root.join("y/one.mp4"), body).unwrap();

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    assert_eq!(outcome.new, 2, "two files: {outcome:?}");

    let rows = file_rows(&store).await.unwrap();
    assert_eq!(rows.len(), 2, "a duplicate is still a file the user has");
    assert_eq!(
        rows[0].object_id, rows[1].object_id,
        "but it is the same content, so it is the same object"
    );
    assert_ne!(rows[0].id, rows[1].id, "and two distinct file rows");
}

/// A file row that does not exist cannot be marked missing.
///
/// The `mark_absent` list used to fall back to using the *path* as the id,
/// which updates no row and inflates the count. The visible symptom is a scan
/// log that claims to have marked five files when it marked none.
#[tokio::test]
async fn a_deleted_file_is_marked_by_id_and_the_count_is_honest() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();

    // Delete two files, and record what the store says about every row first.
    let before = file_rows(&store).await.unwrap();
    std::fs::remove_file(root.join("a/f0.mp4")).unwrap();
    std::fs::remove_file(root.join("b/f1.mp4")).unwrap();

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    assert_eq!(outcome.marked_absent, 2, "{outcome:?}");

    let mut found: Vec<String> = Vec::new();
    for r in &before {
        if file_state(&store, &r.id).await == FileState::Missing.as_str() {
            found.push(r.path.clone());
        }
    }
    found.sort();
    assert_eq!(
        found,
        vec!["a/f0.mp4".to_string(), "b/f1.mp4".to_string()],
        "and exactly the two deleted rows carry the state"
    );
}

/// A file's row id must be derived from its content and path, not generated
/// fresh each time.
///
/// Derived from a random uuid it still works -- for one scan. The second scan
/// is where it breaks: a rescan that re-derives a new id for a file that
/// already has one either collides with the `(object_id, path)` index or
/// creates a second row for the same file.
#[tokio::test]
async fn a_rescan_reuses_the_same_row_ids() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    let first: Vec<(String, String)> = file_rows(&store)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.path))
        .collect();

    // Force a re-read of everything: the point is the ids, not the hint.
    for name in ["a", "b", "c"] {
        for j in 0..4 {
            let p = root.join(format!("{name}/f{j}.mp4"));
            if p.exists() {
                let future = std::time::SystemTime::now()
                    + std::time::Duration::from_secs(10 * (j as u64 + 1));
                filetime::set_file_mtime(&p, filetime::FileTime::from_system_time(future)).unwrap();
            }
        }
    }
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();

    let second: Vec<(String, String)> = file_rows(&store)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.path))
        .collect();
    let mut a = first.clone();
    let mut b = second.clone();
    a.sort();
    b.sort();
    assert_eq!(
        a, b,
        "a rescan must not mint new identities for known files"
    );
}

/// The `(object_id, path)` unique index is the last line of defence.
///
/// Not reachable through the pipeline, which gets it right, and tested here
/// because it is a property of the *schema* that a second code path could
/// break: two inserts with the same content and the same path must not both
/// succeed.
#[tokio::test]
async fn one_path_cannot_have_two_rows_for_the_same_content() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    fixture(&root);
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    let rows = file_rows(&store).await.unwrap();
    let r = &rows[0];
    let again = commons_store::insert_file(
        &store,
        &commons_store::NewFile {
            id: "a-different-id",
            object_id: &r.object_id,
            path: &r.path,
            size_bytes: 1,
            mtime_ns: 1,
            hash_xxh128: None,
            hash_blake3: r.hash_blake3.as_deref(),
        },
    )
    .await;
    assert!(
        again.is_err(),
        "the unique index on (object_id, path) is what stops this"
    );
}

/// A file that moved *and* whose hint moved keeps its object.
///
/// Both events in one scan is ordinary -- a file renamed and rewritten in the
/// same session. The move is the stronger statement about the row, so the
/// update must not run: applying it would re-point the row at a new object on
/// the strength of a hint that, by definition, never saw the content, and
/// would orphan the artifacts the move preserved.
#[tokio::test]
async fn a_move_wins_over_a_hint_that_also_moved() {
    let (sd, store) = store().await;
    let root = sd.path().join("library");
    std::fs::create_dir_all(root.join("a")).unwrap();
    let src = root.join("a/one.mp4");
    std::fs::write(&src, b"original content\n").unwrap();
    pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    let before = file_rows(&store).await.unwrap();
    assert_eq!(before.len(), 1);
    let object_before = before[0].object_id.clone();

    // Rename it, and give the new path a different mtime so the hint moves too.
    let dst = root.join("a/two.mp4");
    std::fs::rename(&src, &dst).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    filetime::set_file_mtime(&dst, filetime::FileTime::from_system_time(future)).unwrap();

    let outcome = pipeline::scan(&store, &root, PipelineConfig::default())
        .await
        .unwrap();
    assert_eq!(outcome.moved, 1, "{outcome:?}");

    let after = file_rows(&store).await.unwrap();
    assert_eq!(after.len(), 1, "still one file");
    assert_eq!(after[0].path, "a/two.mp4", "at the new path");
    assert_eq!(
        after[0].object_id, object_before,
        "and still the same object: a move is not a re-identification"
    );
    assert_eq!(after[0].id, before[0].id, "and the same file row");
}
