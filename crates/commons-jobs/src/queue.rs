//! The durable queue.
//!
//! # The two properties everything else is built on
//!
//! **Submit is idempotent on `dedupe_key`.** A watcher firing fifty times for
//! one file produces one job. This is a `UNIQUE` column in the database doing
//! the work, not a check-then-insert in Rust, because a check-then-insert has
//! a window between the check and the insert and a watcher is exactly the kind
//! of thing that fills it.
//!
//! **Every job eventually becomes terminal.** Not eventually-attempted:
//! terminal. A job that has failed `N` times is `Skipped` and is never picked
//! up again in the same run. This is the poison-pill case, and it is the one
//! that produces infinite loops upstream (stash#2913, #6837): a file that
//! cannot be generated is retried forever, the queue never drains, the CPU is
//! permanently busy, and nothing in the UI says why.
//!
//! The skip list is *per run*, not per queue. A file that failed because a
//! volume was not mounted should be retried next time the volume is there, so
//! "never retried in the same run" is a deliberate and bounded scope.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use commons_core::JobState;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::spec::{Job, JobKind, JobOutcome, JobSpec};

/// How a job may be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Attempts before a job is `Skipped` rather than retried.
    ///
    /// Three by default. One is too few for a transient network blip on a
    /// remote volume; five is too many for a file that does not exist, and the
    /// difference between "slow" and "loops forever" is entirely this number.
    pub max_attempts: u32,
    /// The first retry delay. Doubles per attempt.
    pub base_delay: Duration,
    /// The longest delay between attempts.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(300),
        }
    }
}

impl RetryPolicy {
    /// The delay before attempt number `attempt` (1-based: attempt 1 is the
    /// first retry, i.e. the one after the first failure).
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(32);
        let secs = self
            .base_delay
            .as_secs()
            .saturating_mul(2u64.saturating_pow(exponent));
        Duration::from_secs(secs).min(self.max_delay)
    }
}

/// What the queue is configured to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    pub retry: RetryPolicy,
    /// How many jobs may run at once, across all kinds.
    pub max_concurrent: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        QueueConfig {
            retry: RetryPolicy::default(),
            // Rayon's default for a library this size, and low enough that a
            // 100k-item scan does not saturate a laptop.
            max_concurrent: 4,
        }
    }
}

/// A snapshot of the queue, for the UI and for tests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueStats {
    pub queued: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: usize,
    /// Jobs set aside because they hit the attempt limit.
    pub poisoned: usize,
    /// How many submissions were deduplicated away.
    pub deduplicated: u64,
    /// How many times the queue has been told to claim a job.
    pub claims: u64,
}

impl QueueStats {
    /// Every job the queue is accounting for.
    ///
    /// This is the cross-check that catches a miscount: it must equal
    /// `jobs().len()` at every point in a queue's life, and the two drift
    /// apart the moment a state transition updates one and not the other.
    pub const fn total(&self) -> usize {
        self.queued + self.running + self.done + self.failed + self.skipped + self.cancelled
    }

    /// Is there nothing left to do?
    pub fn is_drained(&self) -> bool {
        self.queued == 0 && self.running == 0
    }

    /// A one-line summary, for a log or a status bar.
    pub fn one_line(&self) -> String {
        if self.is_drained() {
            return format!(
                "done: {} done, {} failed, {} skipped",
                self.done, self.failed, self.skipped
            );
        }
        format!(
            "{} queued, {} running, {} done, {} failed, {} skipped",
            self.queued, self.running, self.done, self.failed, self.skipped
        )
    }
}

/// Everything mutable about the queue.
///
/// Behind a `Mutex` rather than a channel, because the queue has to be
/// inspectable and steerable from another thread while workers are running:
/// `cancel_all` and `stats` are both called from the UI thread, and a queue
/// whose state lives in a channel cannot answer either.
#[derive(Debug)]
struct Inner {
    jobs: BTreeMap<String, Job>,
    /// Submission order. The only fair order for equally-eligible jobs.
    ///
    /// Without this the claim order falls back to the id, and a queue serving
    /// `generate:a` before `generate:b` will serve `generate:a` *forever*:
    /// every time a slot frees, the alphabetically-first job is eligible
    /// again. With one worker that starves every other job in the queue, and
    /// with 100k generate jobs from one scan the user watches the same file
    /// being regenerated over and over.
    sequence: u64,
    /// Submission order per job id.
    order: HashMap<String, u64>,
    /// Jobs whose retry delay has not elapsed, by the time they become ready.
    delayed: Vec<(SystemTime, String)>,
    /// The per-run skip list: jobs that have failed too many times.
    ///
    /// This is what makes a poison pill leave the queue. It is a set of job
    /// ids rather than a flag on the job so that clearing it — which is what
    /// starting a new run does — is one operation and cannot half-apply.
    skip_list: HashSet<String>,
    running: HashSet<String>,
    stats: QueueStats,
}

/// The durable job queue.
///
/// "Durable" here means the *set* of jobs and their terminal states are the
/// caller's to persist (see `snapshot` and `restore`); the queue itself is
/// in-memory and rebuilds from that. A queue that wrote to the database on
/// every state change would make the database a bottleneck for what is
/// fundamentally a scheduling problem, and the interesting durability
/// question — "what was in flight when the process died" — is answered by the
/// `Running` rows, not by a write-ahead log.
#[derive(Debug, Clone)]
pub struct JobQueue {
    inner: Arc<Mutex<Inner>>,
    config: QueueConfig,
    /// Bumped by every submit, so a caller waiting for work has something to
    /// wait on without polling.
    generation: Arc<Mutex<u64>>,
}

impl Default for JobQueue {
    fn default() -> Self {
        JobQueue::new(QueueConfig::default())
    }
}

impl JobQueue {
    /// A queue with the given configuration.
    pub fn new(config: QueueConfig) -> Self {
        JobQueue {
            inner: Arc::new(Mutex::new(Inner {
                jobs: BTreeMap::new(),
                sequence: 0,
                order: HashMap::new(),
                delayed: Vec::new(),
                skip_list: HashSet::new(),
                running: HashSet::new(),
                stats: QueueStats::default(),
            })),
            config,
            generation: Arc::new(Mutex::new(0)),
        }
    }

    /// The configuration.
    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    /// Submit a job.
    ///
    /// Returns the job, and whether it was newly created. A submission whose
    /// `dedupe_key` is already present returns the existing job with
    /// `created: false` and changes nothing — not even the target or the
    /// payload, because a watcher event that arrives *while* a job is running
    /// must not mutate the job that is running.
    pub fn submit(&self, spec: JobSpec) -> (Job, bool) {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.jobs.get(&spec.dedupe_key) {
            let job = existing.clone();
            inner.stats.deduplicated += 1;
            return (job, false);
        }
        let job = Job {
            id: spec.dedupe_key.clone(),
            kind: spec.kind,
            state: JobState::Queued,
            dedupe_key: spec.dedupe_key.clone(),
            target_id: spec.target_id,
            attempts: 0,
            last_error: None,
            payload: spec.payload,
        };
        let seq = inner.sequence;
        inner.sequence += 1;
        inner.order.insert(job.dedupe_key.clone(), seq);
        inner.jobs.insert(job.dedupe_key.clone(), job.clone());
        inner.stats.queued += 1;
        *self.generation.lock() += 1;
        (job, true)
    }

    /// Submit many, and say how many were new.
    ///
    /// This is the watcher path. Fifty events for one file, one job.
    pub fn submit_all(&self, specs: impl IntoIterator<Item = JobSpec>) -> usize {
        let mut created = 0;
        for spec in specs {
            if self.submit(spec).1 {
                created += 1;
            }
        }
        created
    }

    /// Claim the next runnable job, if there is one.
    ///
    /// Honours three things a naive queue does not: the global concurrency
    /// limit, the per-kind limit, and the skip list. A job on the skip list is
    /// never returned even if it is the oldest — that is the entire mechanism.
    pub fn claim(&self, now: SystemTime) -> Option<Job> {
        let mut inner = self.inner.lock();
        promote_ready(&mut inner, now);
        if inner.running.len() >= self.config.max_concurrent {
            return None;
        }
        // One set lookup per claim rather than a scan per candidate job: a
        // queue with 100k delayed jobs and 4 workers would otherwise do 400k
        // comparisons on every claim.
        let waiting: HashSet<&str> = inner.delayed.iter().map(|(_, id)| id.as_str()).collect();
        // Which (kind, target) pairs are already being worked on. Two jobs for
        // the same file and the same kind must not run together; two jobs for
        // two different files must.
        // Only jobs that name a target participate. A job with no target --
        // a whole-library scan, a backup run -- is not about one file, so it
        // cannot conflict with another such job and must not be counted as
        // busy. Counting `None` as a target would serialise every untargeted
        // job in the queue behind the first one, which is the opposite of
        // what "unbounded" is for.
        let busy: HashSet<(JobKind, String)> = inner
            .running
            .iter()
            .filter_map(|id| inner.jobs.get(id))
            .filter_map(|j| j.target_id.clone().map(|t| (j.kind, t)))
            .collect();

        let claimable = inner
            .jobs
            .values()
            .filter(|job| job.state == JobState::Queued)
            .filter(|job| !inner.skip_list.contains(&job.id))
            // A job waiting out a retry delay is queued but not *ready*. This
            // check is the whole of the backoff: without it a delayed job is
            // claimed on the very next `claim` and the delay does nothing.
            // Checking at claim time rather than transitioning to a "waiting"
            // state keeps one source of truth -- the `delayed` list -- instead
            // of two that can disagree.
            .filter(|job| !waiting.contains(job.id.as_str()))
            // One job per (kind, target): a file is never generated twice at
            // once, and never generated and transcoded at once.
            .filter(|job| match &job.target_id {
                Some(target) => !busy.contains(&(job.kind, target.clone())),
                None => true,
            })
            // Fewest attempts first, so a job that has already failed twice
            // is not perpetually beaten by fresh ones, then submission order.
            // Ties on both fall back to the id, which only matters for two
            // jobs submitted in the same nanosecond and makes the order total.
            .min_by(|a, b| {
                a.attempts
                    .cmp(&b.attempts)
                    .then_with(|| inner.order.get(&a.id).cmp(&inner.order.get(&b.id)))
                    .then_with(|| a.id.cmp(&b.id))
            })
            .cloned();

        let job = claimable?;
        inner.running.insert(job.id.clone());
        if let Some(stored) = inner.jobs.get_mut(&job.id) {
            stored.state = JobState::Running;
        }
        inner.stats.queued -= 1;
        inner.stats.running += 1;
        inner.stats.claims += 1;
        Some(Job {
            state: JobState::Running,
            ..job
        })
    }

    /// Record the result of a job.
    ///
    /// The whole retry-and-skip policy lives here, and it is one function so
    /// that there is nowhere else it could be got wrong.
    pub fn complete(&self, job_id: &str, outcome: JobOutcome, now: SystemTime) -> Option<Job> {
        let mut inner = self.inner.lock();
        let job = inner.jobs.get(job_id)?.clone();
        if !inner.running.remove(job_id) {
            // Completing a job that is not running means a worker crashed
            // without reporting and something is completing it twice. The
            // first completion wins; the second is ignored rather than
            // counted, because a double-counted `done` is a progress bar that
            // goes past 100%.
            return None;
        }
        inner.stats.running -= 1;

        let mut stored = job.clone();
        stored.attempts += 1;
        stored.last_error = outcome.error.clone();

        let exhausted = stored.attempts >= self.config.retry.max_attempts;
        let retryable = outcome.state == JobState::Queued;
        if retryable && exhausted {
            // The poison pill. Marked skipped, never offered again this run.
            stored.state = JobState::Skipped;
            inner.skip_list.insert(job_id.to_string());
            inner.stats.poisoned += 1;
            inner.stats.skipped += 1;
        } else if retryable {
            stored.state = JobState::Queued;
            let delay = outcome
                .retry_in
                .unwrap_or_else(|| self.config.retry.delay_for(stored.attempts));
            inner.delayed.push((now + delay, job_id.to_string()));
            inner.stats.queued += 1;
        } else {
            stored.state = outcome.state;
            match outcome.state {
                JobState::Done => inner.stats.done += 1,
                JobState::Failed => inner.stats.failed += 1,
                JobState::Skipped => inner.stats.skipped += 1,
                JobState::Cancelled => inner.stats.cancelled += 1,
                // A handler returning `Queued` with no delay is handled above;
                // anything else is a state the queue does not produce.
                JobState::Queued | JobState::Running => inner.stats.running += 1,
            }
        }
        inner.jobs.insert(job_id.to_string(), stored.clone());
        *self.generation.lock() += 1;
        Some(stored)
    }

    /// Is this job on the skip list?
    pub fn is_skipped(&self, job_id: &str) -> bool {
        self.inner.lock().skip_list.contains(job_id)
    }

    /// The skip list, for a status line or a bug report.
    pub fn skipped_jobs(&self) -> Vec<Job> {
        let inner = self.inner.lock();
        inner
            .skip_list
            .iter()
            .filter_map(|id| inner.jobs.get(id))
            .cloned()
            .collect()
    }

    /// Clear the skip list and requeue everything on it. This is a new run.
    pub fn clear_skip_list(&self) -> usize {
        let mut inner = self.inner.lock();
        let ids: Vec<String> = inner.skip_list.iter().cloned().collect();
        let n = ids.len();
        for id in &ids {
            inner.skip_list.remove(id);
            if let Some(job) = inner.jobs.get_mut(id) {
                if job.state == JobState::Skipped {
                    job.state = JobState::Queued;
                    job.attempts = 0;
                    job.last_error = None;
                    inner.stats.skipped -= 1;
                    inner.stats.queued += 1;
                }
            }
        }
        if n > 0 {
            *self.generation.lock() += 1;
        }
        n
    }

    /// Cancel one job. A running job is marked cancelled; the worker will
    /// find out when it tries to report, and its report is ignored.
    pub fn cancel(&self, job_id: &str) -> bool {
        let mut inner = self.inner.lock();
        let Some(job) = inner.jobs.get(job_id) else {
            return false;
        };
        match job.state {
            JobState::Queued | JobState::Running => {
                let was_running = inner.running.remove(job_id);
                if let Some(stored) = inner.jobs.get_mut(job_id) {
                    stored.state = JobState::Cancelled;
                }
                if was_running {
                    inner.stats.running -= 1;
                } else {
                    inner.stats.queued -= 1;
                }
                inner.stats.cancelled += 1;
                *self.generation.lock() += 1;
                true
            }
            _ => false,
        }
    }

    /// Cancel everything not yet finished.
    pub fn cancel_all(&self) -> usize {
        let ids: Vec<String> = {
            let inner = self.inner.lock();
            inner
                .jobs
                .values()
                .filter(|j| matches!(j.state, JobState::Queued | JobState::Running))
                .map(|j| j.id.clone())
                .collect()
        };
        ids.iter().filter(|id| self.cancel(id)).count()
    }

    /// A job by id.
    pub fn get(&self, job_id: &str) -> Option<Job> {
        self.inner.lock().jobs.get(job_id).cloned()
    }

    /// Every job, in a stable order.
    pub fn jobs(&self) -> Vec<Job> {
        self.inner.lock().jobs.values().cloned().collect()
    }

    /// Jobs in one state.
    pub fn jobs_in(&self, state: JobState) -> Vec<Job> {
        self.inner
            .lock()
            .jobs
            .values()
            .filter(|j| j.state == state)
            .cloned()
            .collect()
    }

    /// Requeue one job, clearing its attempt count and taking it off the
    /// skip list.
    ///
    /// The "new run" for a single job. Refuses a job that is running: a
    /// running job's attempt count belongs to the attempt in progress, and
    /// zeroing it would let a job that fails every time never reach the
    /// limit.
    pub fn resubmit(&self, job_id: &str) -> bool {
        let mut inner = self.inner.lock();
        let Some(job) = inner.jobs.get(job_id).cloned() else {
            return false;
        };
        if job.state == JobState::Running {
            return false;
        }
        let was_terminal = job.is_terminal();
        inner.skip_list.remove(job_id);
        if let Some(stored) = inner.jobs.get_mut(job_id) {
            stored.state = JobState::Queued;
            stored.attempts = 0;
            stored.last_error = None;
        }
        if was_terminal {
            match job.state {
                JobState::Done => inner.stats.done = inner.stats.done.saturating_sub(1),
                JobState::Failed => inner.stats.failed = inner.stats.failed.saturating_sub(1),
                JobState::Skipped => {
                    inner.stats.skipped -= 1;
                    // It was a poison pill, and it is being given another go.
                    inner.stats.poisoned = inner.stats.poisoned.saturating_sub(1);
                }
                JobState::Cancelled => inner.stats.cancelled -= 1,
                JobState::Queued | JobState::Running => {}
            }
        }
        inner.stats.queued += 1;
        *self.generation.lock() += 1;
        true
    }

    /// A snapshot of the counters.
    pub fn stats(&self) -> QueueStats {
        self.inner.lock().stats.clone()
    }

    /// Is there nothing left to do?
    pub fn is_drained(&self) -> bool {
        let inner = self.inner.lock();
        inner.jobs.values().all(|j| j.is_terminal()) && inner.delayed.is_empty()
    }

    /// How many submissions have been made. A cheap change detector, so a
    /// supervisor can wait for work without a timer.
    pub fn generation(&self) -> u64 {
        *self.generation.lock()
    }

    /// Everything, for persistence. The queue's whole state.
    pub fn snapshot(&self) -> QueueSnapshot {
        let inner = self.inner.lock();
        QueueSnapshot {
            jobs: inner.jobs.values().cloned().collect(),
            skip_list: inner.skip_list.iter().cloned().collect(),
            delayed: inner.delayed.clone(),
        }
    }

    /// Rebuild a queue from a snapshot, treating everything that was running
    /// as queued again.
    ///
    /// This is stash#1445, and the one decision worth arguing about is what to
    /// do with jobs that were `Running` when the process died. The answer is:
    /// requeue them, and do *not* count the attempt they were partway through.
    /// A job killed by a crash did not fail, and counting it would mean a
    /// process that crashes repeatedly makes no progress on a job that works
    /// fine — it would reach the attempt limit and be skipped for a reason
    /// that has nothing to do with the job.
    pub fn restore(snapshot: QueueSnapshot, config: QueueConfig) -> Self {
        let queue = JobQueue::new(config);
        {
            let mut inner = queue.inner.lock();
            for mut job in snapshot.jobs {
                // A job that was in flight when the process died goes back on
                // the queue. A job that had already finished stays finished --
                // and the counters are rebuilt from the states rather than
                // assumed, because assuming is how a restored queue ends up
                // claiming 3 queued jobs while holding 2.
                if job.state == JobState::Running {
                    job.state = JobState::Queued;
                }
                // `Running` is unreachable here -- the branch above has
                // already turned every in-flight job into `Queued`. It is
                // still matched, rather than left out, so that adding a
                // state to `JobState` is a compile error at every place that
                // counts states instead of a silently uncounted one.
                match job.state {
                    JobState::Queued => inner.stats.queued += 1,
                    JobState::Running => unreachable!("handled above"),
                    JobState::Done => inner.stats.done += 1,
                    JobState::Failed => inner.stats.failed += 1,
                    JobState::Skipped => inner.stats.skipped += 1,
                    JobState::Cancelled => inner.stats.cancelled += 1,
                }
                let seq = inner.sequence;
                inner.sequence += 1;
                inner.order.insert(job.dedupe_key.clone(), seq);
                inner.jobs.insert(job.dedupe_key.clone(), job);
            }
            inner.skip_list = snapshot.skip_list.into_iter().collect();
            inner.delayed = snapshot.delayed;
            // A restored `Running` row means the process died mid-job, so
            // those attempts never reported. Not counting them is the whole
            // reason resume works rather than merely restarting: a process
            // that crashes repeatedly makes progress instead of burning
            // through the retry limit on jobs that work fine.
        }
        queue
    }
}

/// The queue's whole state, for persistence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub jobs: Vec<Job>,
    pub skip_list: HashSet<String>,
    /// Jobs waiting out a retry delay, by the time they become ready.
    pub delayed: Vec<(SystemTime, String)>,
}

/// Move delayed jobs whose time has come back into the runnable set.
fn promote_ready(inner: &mut Inner, now: SystemTime) {
    if inner.delayed.is_empty() {
        return;
    }
    let (ready, still_waiting): (Vec<_>, Vec<_>) =
        inner.delayed.drain(..).partition(|(at, _)| *at <= now);
    inner.delayed = still_waiting;
    for (_, id) in ready {
        if inner.skip_list.contains(&id) {
            // It hit the limit while it was waiting. Do not resurrect it.
            continue;
        }
    }
}
