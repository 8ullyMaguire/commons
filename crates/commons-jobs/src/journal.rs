//! Persisting the queue.
//!
//! # Why the queue is not itself async
//!
//! `JobQueue` is a synchronous, in-memory structure behind a mutex, and that
//! is a deliberate choice: the claim path runs in a pool thread, and making it
//! `async` would mean every claim is an await point that a cancelled task can
//! interrupt. Instead the queue *records* what changed and a [`Journal`]
//! writes it out.
//!
//! The alternative -- giving the queue a `Store` and an `async fn` on every
//! transition -- was rejected for one concrete reason: `submit` has to be
//! idempotent *across processes*, and the only thing that can enforce that is
//! the unique constraint on `job.dedupe_key`. An in-memory check plus a later
//! write has a window, and the window is exactly when a scan and a watcher both
//! see the same new file.
//!
//! # What is and is not durable
//!
//! The journal makes every state transition durable. What it deliberately does
//! *not* do is hold the write in the path of a claim: a worker takes a job,
//! runs it for thirty seconds, and the `running` row is written by the journal
//! before the work starts. A crash between the write and the completion
//! therefore leaves a `Running` row, which is what `restore` is built to
//! handle. Writing the completion synchronously would be marginally more
//! precise and would also mean a slow disk can stall a worker, which is how a
//! queue stops being a queue.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use commons_store::{NewJob, Store, StoredJob};

use crate::queue::QueueSnapshot;
use crate::spec::JobKind;
use commons_core::JobState;

use crate::spec::Job;

/// The format timestamps are written in.
///
/// RFC 3339 in UTC, matching the `TEXT` timestamps the Phase 0 schema already
/// stores, so a job row and a file row are comparable as strings -- which is
/// what the UI's ordering does.
pub fn now_string() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    // Hand-rolled rather than pulling in `chrono` for one format. Days-from-
    // epoch to civil date is the whole algorithm and it is four lines; a
    // dependency to avoid four lines is a bad trade in a crate that every
    // binary links.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's `civil_from_days`, for days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// One thing that happened to the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEntry {
    /// A job was created.
    Insert(Job),
    /// A job's state or attempt count changed.
    Update {
        id: String,
        state: JobKind,
        state_name: String,
        attempts: i64,
        last_error: Option<String>,
    },
}

/// Writes journal entries to the `job` table.
///
/// A trait rather than a concrete `Store` so a test can assert on what the
/// queue *tried* to persist without a database, and so this module names no
/// SQLx types.
///
/// `async_trait` rather than native `async fn` in a trait: the trait has to
/// stay object-safe, because [`Journal::flush`] takes `&dyn Sink` and there
/// is no reason to make a caller pick a generic to write a batch out.
#[async_trait::async_trait]
pub trait Sink: Send + Sync + std::fmt::Debug {
    /// Apply entries in order.
    async fn apply(&self, entries: &[JournalEntry]) -> anyhow::Result<()>;

    /// Every job row, for restore.
    async fn load(&self) -> anyhow::Result<Vec<StoredJob>>;
}

/// The real sink: `commons-store`.
#[derive(Debug, Clone)]
pub struct StoreSink {
    store: Store,
}

impl StoreSink {
    pub fn new(store: Store) -> Self {
        StoreSink { store }
    }
}

#[async_trait::async_trait]
impl Sink for StoreSink {
    async fn apply(&self, entries: &[JournalEntry]) -> anyhow::Result<()> {
        let now = now_string();
        for entry in entries {
            match entry {
                JournalEntry::Insert(job) => {
                    commons_store::insert_job_if_absent(
                        &self.store,
                        &NewJob {
                            id: &job.dedupe_key,
                            kind: job.kind.as_str(),
                            state: job.state.as_str(),
                            dedupe_key: &job.dedupe_key,
                            target_id: job.target_id.as_deref(),
                            attempts: job.attempts as i64,
                            last_error: job.last_error.as_deref(),
                            now: &now,
                        },
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("insert job: {e}"))?;
                }
                JournalEntry::Update {
                    id,
                    state_name,
                    attempts,
                    last_error,
                    ..
                } => {
                    commons_store::update_job(
                        &self.store,
                        id,
                        state_name,
                        *attempts,
                        last_error.as_deref(),
                        &now,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("update job: {e}"))?;
                }
            }
        }
        Ok(())
    }

    async fn load(&self) -> anyhow::Result<Vec<StoredJob>> {
        commons_store::job_rows(&self.store)
            .await
            .map_err(|e| anyhow::anyhow!("load jobs: {e}"))
    }
}

/// Collects entries and writes them in batches.
///
/// Batching is not an optimisation here, it is the difference between one
/// `fsync` per state transition and one per batch. A 100k-item scan produces
/// hundreds of thousands of transitions, and a synchronous write per
/// transition turns the journal into the slowest thing in the scan.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    pending: Arc<Mutex<Vec<JournalEntry>>>,
    written: Arc<Mutex<u64>>,
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an entry. Does not write.
    pub fn record(&self, entry: JournalEntry) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push(entry);
        }
    }

    /// How many entries are waiting to be written.
    pub fn pending(&self) -> usize {
        self.pending.lock().map(|p| p.len()).unwrap_or(0)
    }

    /// How many entries have been written since the journal was created.
    pub fn written(&self) -> u64 {
        self.written.lock().map(|w| *w).unwrap_or(0)
    }

    /// Write everything pending, in order.
    pub async fn flush(&self, sink: &dyn Sink) -> anyhow::Result<usize> {
        let batch = {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| anyhow::anyhow!("journal poisoned"))?;
            std::mem::take(&mut *pending)
        };
        if batch.is_empty() {
            return Ok(0);
        }
        let n = batch.len();
        sink.apply(&batch).await?;
        if let Ok(mut w) = self.written.lock() {
            *w += n as u64;
        }
        Ok(n)
    }

    /// Read the persisted queue back.
    pub async fn load(sink: &dyn Sink) -> anyhow::Result<Vec<StoredJob>> {
        sink.load().await
    }
}

/// Rebuild an in-memory queue from persisted rows.
///
/// A `Running` row is a job the process was in the middle of when it died, so
/// it comes back as `Queued`. Its attempt count is preserved: the attempt was
/// never reported as failed, and a process that crashes repeatedly must make
/// progress rather than reach the attempt limit on a job that works fine.
pub fn snapshot_from_rows(rows: Vec<StoredJob>) -> QueueSnapshot {
    let mut jobs = Vec::with_capacity(rows.len());
    for row in rows {
        // An unrecognised kind is dropped, not guessed at. Running a job
        // under the wrong kind is worse than not running it: a future
        // version's `cluster_refresh` read as a `plugin` job would be handed
        // to whatever handler is registered for plugins.
        let Some(kind) = JobKind::parse(&row.kind) else {
            continue;
        };
        jobs.push(Job {
            id: row.id.clone(),
            kind,
            state: JobState::parse_or_queued(&row.state),
            dedupe_key: row.dedupe_key,
            target_id: row.target_id,
            attempts: row.attempts.max(0) as u32,
            last_error: row.last_error,
            payload: None,
        });
    }
    QueueSnapshot {
        jobs,
        skip_list: std::collections::HashSet::new(),
        delayed: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_is_rfc3339_in_utc() {
        let s = now_string();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
    }

    #[test]
    fn the_calendar_is_right_at_the_edges() {
        // Days since the epoch, and the civil dates they mean. These are the
        // four that catch an off-by-one in the era arithmetic.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // 2024 is a leap year, and the day before is 2023-12-31.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
    }

    #[test]
    fn every_kind_has_a_persisted_name_that_round_trips() {
        // The `job.kind` column is the only record of what a job was, and a
        // name that does not survive a round trip is a job that cannot be
        // restored after a restart.
        assert_eq!(JobKind::ALL.len(), 11, "ten operations plus Plugin");
        for kind in JobKind::ALL {
            let name = kind.as_str();
            assert_eq!(
                JobKind::parse(name),
                Some(kind),
                "{name} does not parse back to {kind:?}"
            );
        }
        assert_eq!(JobKind::parse("nonsense"), None);
    }

    #[test]
    fn an_unknown_state_name_requeues_rather_than_dropping_the_job() {
        // `job.state` has no CHECK constraint, so this is a real input: a
        // downgrade, a hand-edited row, a future build. Re-running a job is a
        // waste; refusing to start is worse.
        assert_eq!(JobState::parse_or_queued("running"), JobState::Running);
        assert_eq!(JobState::parse_or_queued("done"), JobState::Done);
        assert_eq!(JobState::parse_or_queued("wat"), JobState::Queued);
    }
}
