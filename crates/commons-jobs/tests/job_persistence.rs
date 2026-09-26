//! T-P2-004's durability: the queue survives the process, not just a restart
//! of a `JobQueue` value.
//!
//! `queue_acceptance.rs` proves resume from a `QueueSnapshot`. That is a
//! snapshot of the same in-memory structure, so it can only prove the
//! *reconstruction* is right. These tests go through SQLite, because "durable"
//! has to mean the rows are in the database a second process will open —
//! otherwise the honest description of the ticket's "one durable queue" is "one
//! queue, as long as nothing restarts".

use std::time::{Duration, SystemTime};

use commons_jobs::journal::{
    now_string, snapshot_from_rows, Journal, JournalEntry, Sink, StoreSink,
};
use commons_jobs::spec::JobSpec;
use commons_jobs::{JobKind, JobQueue, QueueConfig, RetryPolicy};
use commons_store::{insert_job_if_absent, job_rows, NewJob, Store};
use sqlx::Row;

use commons_jobs::worker::ok_handler;

fn config() -> QueueConfig {
    QueueConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        },
        max_concurrent: 4,
    }
}

async fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    (dir, store)
}

/// A submitted job reaches the `job` table.
#[tokio::test]
async fn a_submitted_job_is_a_row() {
    let (_dir, store) = store().await;
    let sink = StoreSink::new(store.clone());
    let journal = Journal::new();
    let queue = JobQueue::new(config());

    journal.record(JournalEntry::Insert(
        queue
            .submit(JobSpec::new(JobKind::Transcode, "movie.mkv"))
            .0,
    ));
    assert_eq!(journal.flush(&sink).await.unwrap(), 1);

    let rows = sink.load().await.unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].kind, "transcode");
    assert_eq!(rows[0].state, "queued");
    assert_eq!(rows[0].target_id.as_deref(), Some("movie.mkv"));
    assert_eq!(rows[0].attempts, 0);

    // The timestamp that was actually written, not just its shape: a row whose
    // `created_at` is empty still sorts and still restores, and quietly breaks
    // every "when did this finish" question the UI asks.
    let created: String = sqlx::query("SELECT created_at FROM job WHERE id = ?")
        .bind("transcode:movie.mkv")
        .fetch_one(store.sqlite_pool().unwrap())
        .await
        .expect("row")
        .get("created_at");
    assert_eq!(created, now_string());
}

/// `insert_job_if_absent` says whether it created a row, and that answer is
/// the only thing a caller has to distinguish "queued" from "already known".
#[tokio::test]
async fn insert_reports_whether_it_created_the_row() {
    let (_dir, store) = store().await;
    let now = now_string();
    let job = |id: &'static str| NewJob {
        id,
        kind: "scan",
        state: "queued",
        dedupe_key: "scan:library",
        target_id: None,
        attempts: 0,
        last_error: None,
        now: &now,
    };

    assert!(
        insert_job_if_absent(&store, &job("a")).await.unwrap(),
        "the first insert creates"
    );
    assert!(
        !insert_job_if_absent(&store, &job("b")).await.unwrap(),
        "the second does not, and says so"
    );
    let rows = job_rows(&store).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "a", "and the original row is untouched");
}

/// The dedupe key is unique *in the database*, not just in memory.
///
/// This is the property that makes submit idempotent across processes, and it
/// is the reason `insert_job_if_absent` is one statement: check-then-insert has
/// a window, and the window is exactly when a scan and a watcher both see the
/// same new file.
#[tokio::test]
async fn the_dedupe_key_is_unique_in_the_database() {
    let (_dir, store) = store().await;
    let sink = StoreSink::new(store.clone());

    // Two *separate* queues, as two processes would have.
    for _ in 0..2 {
        let queue = JobQueue::new(config());
        let journal = Journal::new();
        journal.record(JournalEntry::Insert(
            queue.submit(JobSpec::new(JobKind::Generate, "a.mp4")).0,
        ));
        journal.flush(&sink).await.unwrap();
    }

    let rows = sink.load().await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a second submit of the same key must not create a second row: {rows:?}"
    );
}

/// A thousand duplicate keys are one row, through the real constraint.
#[tokio::test]
async fn a_thousand_duplicate_keys_are_one_row() {
    let (_dir, store) = store().await;
    let sink = StoreSink::new(store.clone());
    let queue = JobQueue::new(config());
    let journal = Journal::new();

    // Every submit is journalled, including the duplicates -- the point is
    // that the *database* collapses them, not that the caller checks first.
    for _ in 0..1_000 {
        journal.record(JournalEntry::Insert(
            queue.submit(JobSpec::new(JobKind::Scan, "library")).0,
        ));
    }
    assert_eq!(
        journal.flush(&sink).await.unwrap(),
        1_000,
        "all thousand entries were written"
    );

    assert_eq!(
        sink.load().await.unwrap().len(),
        1,
        "and the unique index collapsed them to one row"
    );
    assert_eq!(queue.stats().deduplicated, 999);
}

/// A crash mid-job leaves a `Running` row, and restore puts it back.
#[tokio::test]
async fn a_running_row_comes_back_queued_after_a_crash() {
    let (_dir, store) = store().await;
    let sink = StoreSink::new(store.clone());
    let queue = JobQueue::new(config());
    let journal = Journal::new();
    for target in ["a", "b"] {
        journal.record(JournalEntry::Insert(
            queue.submit(JobSpec::new(JobKind::Generate, target)).0,
        ));
    }
    journal.flush(&sink).await.unwrap();

    // One job finishes, the next is in flight when the process dies.
    let done = queue.claim(SystemTime::now()).unwrap();
    queue.complete(
        &done.id,
        commons_jobs::JobOutcome::done(),
        SystemTime::now(),
    );
    journal.record(JournalEntry::Update {
        id: done.id.clone(),
        state: done.kind,
        state_name: "done".into(),
        attempts: 1,
        last_error: None,
    });
    journal.flush(&sink).await.unwrap();

    let in_flight = queue.claim(SystemTime::now()).unwrap();
    journal.record(JournalEntry::Update {
        id: in_flight.id.clone(),
        state: in_flight.kind,
        state_name: "running".into(),
        attempts: 0,
        last_error: None,
    });
    journal.flush(&sink).await.unwrap();

    // A second process opens the same database.
    let reopened = Store::open_library(_dir.path()).await.unwrap();
    let rows = StoreSink::new(reopened).load().await.unwrap();
    let snapshot = snapshot_from_rows(rows);
    let restored = JobQueue::restore(snapshot, config());

    assert_eq!(restored.get(&done.id).unwrap().state.as_str(), "done");
    let resumed = restored.get(&in_flight.id).unwrap();
    assert_eq!(
        resumed.state.as_str(),
        "queued",
        "a job the process was in the middle of is queued again"
    );
    assert_eq!(
        resumed.attempts, 0,
        "a crash is not a failure and does not count against the retry limit"
    );

    // And it drains.
    let mut handlers = std::collections::HashMap::new();
    handlers.insert(JobKind::Generate, ok_handler());
    handlers.insert(JobKind::Scan, ok_handler());
    let pool = commons_jobs::WorkerPool::new(restored.clone(), handlers);
    pool.drain_blocking(20);
    assert!(restored.is_drained(), "{}", restored.stats().one_line());
    assert_eq!(restored.stats().done, 2);
}

/// Timestamps sort as strings, which is what the UI's ordering relies on.
#[tokio::test]
async fn timestamps_are_sortable_and_round_trip() {
    let (_dir, store) = store().await;
    let sink = StoreSink::new(store.clone());
    let queue = JobQueue::new(config());
    let journal = Journal::new();
    for target in ["a", "b", "c"] {
        journal.record(JournalEntry::Insert(
            queue.submit(JobSpec::new(JobKind::Backup, target)).0,
        ));
    }
    journal.flush(&sink).await.unwrap();

    let raw: Vec<String> = sqlx::query("SELECT created_at FROM job")
        .fetch_all(store.sqlite_pool().unwrap())
        .await
        .expect("query")
        .into_iter()
        .map(|r| r.get::<String, _>("created_at"))
        .collect();
    let mut sorted = raw.clone();
    sorted.sort();
    assert_eq!(raw, sorted, "lexicographic order is chronological: {raw:?}");
    assert!(raw.iter().all(|s| s.starts_with("20")), "{raw:?}");
}

/// A row in a state this build does not know is requeued, not lost.
#[tokio::test]
async fn a_state_from_a_future_version_is_requeued_not_dropped() {
    let (_dir, store) = store().await;
    sqlx::query(
        "INSERT INTO job (id, kind, state, dedupe_key, attempts, created_at, updated_at)
         VALUES ('j2', 'scan', 'hibernating', 'scan:j2', 0, ?, ?)",
    )
    .bind(now_string())
    .bind(now_string())
    .execute(store.sqlite_pool().unwrap())
    .await
    .expect("insert");

    let rows = StoreSink::new(store).load().await.unwrap();
    let snapshot = snapshot_from_rows(rows);
    assert_eq!(
        snapshot.jobs.len(),
        1,
        "the job survives: {:?}",
        snapshot.jobs
    );
    assert_eq!(
        snapshot.jobs[0].state.as_str(),
        "queued",
        "re-running a job is a waste; refusing to start is worse"
    );
}

/// A row whose kind this build does not know is dropped on restore, not
/// guessed at.
#[tokio::test]
async fn an_unknown_kind_is_not_run_as_something_else() {
    let (_dir, store) = store().await;
    sqlx::query(
        "INSERT INTO job (id, kind, state, dedupe_key, attempts, created_at, updated_at)
         VALUES ('j1', 'quantum_reindex', 'queued', 'quantum:1', 0, ?, ?)",
    )
    .bind(now_string())
    .bind(now_string())
    .execute(store.sqlite_pool().unwrap())
    .await
    .expect("insert");

    let rows = StoreSink::new(store).load().await.unwrap();
    let snapshot = snapshot_from_rows(rows);
    assert!(
        snapshot.jobs.is_empty(),
        "a job this build cannot run must not be handed to a handler: {:?}",
        snapshot.jobs
    );
}
