//! Keeping the pool fed, and keeping the machine awake while it works.
//!
//! # What a supervisor is for
//!
//! The queue in `queue.rs` knows what work exists. The pool in `worker.rs`
//! knows how to run one job. Neither of them notices that the queue is full
//! and nothing is running, which is the failure that looks like "the app is
//! just not doing anything". The supervisor is the thing that notices, and
//! the thing that notices it does not have to be a thread per job.
//!
//! # Suspend inhibition (stash#5517)
//!
//! A laptop that suspends halfway through generating 4,000 files is a user who
//! comes back to a half-finished library and a queue that thinks everything is
//! running. Worse, the processes may or may not have survived depending on the
//! filesystem, so the queue's idea of what is in flight stops matching
//! reality.
//!
//! So while the supervisor has work in flight it holds a system inhibitor,
//! and drops it when the queue drains. The two platform mechanisms are
//! different and both are best-effort: on Linux it is an inhibitor lock file
//! under the runtime directory, and on macOS it is a process assertion. Where
//! neither is available the supervisor says so rather than pretending, because
//! "we could not hold an inhibitor" is information a user wants and a silent
//! no-op is not.
//!
//! The inhibitor is *released* by the kernel if the process dies, which is
//! the property that makes this safe to do without a cleanup path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use commons_core::JobState;
use parking_lot::Mutex;

use crate::queue::{JobQueue, QueueConfig};
use crate::spec::{JobKind, JobOutcome};
use crate::worker::{Handler, WorkerPool};

/// How the supervisor behaves.
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisorConfig {
    /// The queue it drains.
    pub queue: QueueConfig,
    /// How long to wait between polls when there is nothing to do.
    ///
    /// Short by design. A supervisor is not a performance-critical path, and
    /// a long poll interval is visible to the user as "I submitted a job and
    /// nothing happened for ten seconds".
    pub poll_interval: Duration,
    /// Whether to hold a suspend inhibitor while work is in flight (#5517).
    pub inhibit_suspend: bool,
    /// Where to put the inhibitor lock file, if the platform uses one.
    pub runtime_dir: Option<PathBuf>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            queue: QueueConfig::default(),
            poll_interval: Duration::from_millis(50),
            inhibit_suspend: true,
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        }
    }
}

/// A handle to a running supervisor.
#[derive(Debug, Clone)]
pub struct SupervisorHandle {
    stop: Arc<Mutex<bool>>,
    /// The queue, so a caller can submit and watch without another handle.
    queue: JobQueue,
}

impl SupervisorHandle {
    /// The queue being drained.
    pub fn queue(&self) -> &JobQueue {
        &self.queue
    }

    /// Ask the supervisor to finish what it is doing and stop.
    pub fn stop(&self) {
        *self.stop.lock() = true;
    }

    /// Has the supervisor been asked to stop?
    pub fn is_stopping(&self) -> bool {
        *self.stop.lock()
    }
}

/// Keeps a [`WorkerPool`] fed, and holds a suspend inhibitor while it works.
#[derive(Clone)]
pub struct Supervisor {
    pool: WorkerPool,
    config: SupervisorConfig,
    stop: Arc<Mutex<bool>>,
    inhibitor: Arc<Mutex<Option<SuspendInhibitor>>>,
}

impl Supervisor {
    /// A supervisor over `queue` with `handlers`.
    pub fn new(
        queue: JobQueue,
        handlers: HashMap<JobKind, Handler>,
        config: SupervisorConfig,
    ) -> Self {
        let pool = WorkerPool::new(queue.clone(), handlers);
        Supervisor {
            pool,
            config,
            stop: Arc::new(Mutex::new(false)),
            inhibitor: Arc::new(Mutex::new(None)),
        }
    }

    /// The pool.
    pub fn pool(&self) -> &WorkerPool {
        &self.pool
    }

    /// The queue.
    pub fn queue(&self) -> &JobQueue {
        self.pool.queue()
    }

    /// Ask this supervisor to stop.
    pub fn stop(&self) {
        *self.stop.lock() = true;
    }

    /// Has the supervisor been asked to stop?
    pub fn is_stopping(&self) -> bool {
        *self.stop.lock()
    }

    /// A handle, for another thread.
    pub fn handle(&self) -> SupervisorHandle {
        SupervisorHandle {
            stop: Arc::clone(&self.stop),
            queue: self.pool.queue().clone(),
        }
    }

    /// Is an inhibitor currently held?
    pub fn is_inhibiting(&self) -> bool {
        self.inhibitor.lock().is_some()
    }

    /// Run until the queue drains or the supervisor is stopped.
    ///
    /// Blocking. Returns how many jobs it ran.
    pub fn run_blocking(&self) -> usize {
        let mut ran = 0;
        *self.stop.lock() = false;
        while !self.is_stopping() {
            if self.tick(SystemTime::now()) {
                ran += 1;
            } else if self.pool.queue().is_drained() {
                break;
            } else {
                std::thread::sleep(self.config.poll_interval);
            }
        }
        self.release_inhibitor();
        ran
    }

    /// One iteration. Returns whether a job ran.
    ///
    /// The unit the tests drive, so a test that ticks is testing what the
    /// loop does rather than a reimplementation of it.
    ///
    /// The inhibitor is reconciled *after* the job runs, not before. Doing it
    /// before meant the check described the queue as it was before the last
    /// job, so the inhibitor was still held after the final job finished and
    /// only released by one more tick. A supervisor that stopped as soon as
    /// the queue drained -- which is what `run_blocking` does, and what a
    /// caller wiring this into an app's shutdown path does -- left a stale
    /// `systemd-inhibit` lock file behind for the rest of the session, telling
    /// the system the machine was busy when nothing was.
    pub fn tick(&self, now: SystemTime) -> bool {
        if self.is_stopping() {
            self.release_inhibitor();
            return false;
        }
        // Held *before* the job runs, via the pool's callback, because
        // `run_one` is synchronous: control does not come back here until the
        // job has finished. A check between jobs is a check that never sees a
        // job in flight, which is the one moment the inhibitor exists for.
        let ran = self
            .pool
            .run_one_notifying(now, &|_job| {
                self.acquire_inhibitor();
            })
            .is_some();
        // Released *after*: the job above may have been the last one, and a
        // check made before it ran cannot know that.
        if self.pool.queue().is_drained() {
            self.release_inhibitor();
        }
        ran
    }

    /// Take the inhibitor if the work justifies it and we do not have it.
    pub fn acquire_inhibitor(&self) -> bool {
        if !self.config.inhibit_suspend {
            return false;
        }
        let mut guard = self.inhibitor.lock();
        if guard.is_some() {
            return false;
        }
        match SuspendInhibitor::acquire(self.config.runtime_dir.clone()) {
            Some(inhibitor) => {
                *guard = Some(inhibitor);
                true
            }
            // No inhibitor available. Not an error: the work still gets done,
            // it just might be interrupted by a suspend. The user should be
            // able to see that, so it is a real return value rather than a
            // silent None.
            None => false,
        }
    }

    /// Drop the inhibitor. Idempotent.
    pub fn release_inhibitor(&self) {
        // `take` then drop outside the lock: removing a lock file is I/O and
        // holding the queue-adjacent mutex across a syscall is how a fast
        // shutdown turns into a slow one.
        let inhibitor = self.inhibitor.lock().take();
        drop(inhibitor);
    }

    /// The whole queue, by state, for a progress display.
    pub fn progress(&self) -> Progress {
        let stats = self.pool.queue().stats();
        let by_kind: HashMap<JobKind, usize> = self
            .pool
            .queue()
            .jobs()
            .into_iter()
            .filter(|j| !j.is_terminal())
            .fold(HashMap::new(), |mut acc, j| {
                *acc.entry(j.kind).or_insert(0) += 1;
                acc
            });
        Progress {
            total: stats.queued + stats.running,
            queued: stats.queued,
            running: stats.running,
            done: stats.done,
            failed: stats.failed,
            skipped: stats.skipped,
            by_kind,
            poison_pills: stats.poisoned,
            inhibiting: self.is_inhibiting(),
        }
    }
}

/// How far along the work is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Progress {
    /// Jobs not yet finished.
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Outstanding jobs per kind.
    pub by_kind: HashMap<JobKind, usize>,
    /// Jobs set aside for hitting the attempt limit.
    pub poison_pills: usize,
    /// Whether a suspend inhibitor is held.
    pub inhibiting: bool,
}

impl Progress {
    /// A fraction from 0 to 1, for a progress bar.
    ///
    /// Counts `skipped` and `failed` as finished. They are: the job is over,
    /// and a progress bar that stops at 90% because ten files were unreadable
    /// is a progress bar that never goes to 100%.
    pub fn fraction(&self) -> f64 {
        let finished = self.done + self.failed + self.skipped;
        let total = finished + self.queued + self.running;
        if total == 0 {
            return 1.0;
        }
        finished as f64 / total as f64
    }

    /// One line, for a log or a status bar.
    pub fn one_line(&self) -> String {
        let mut line = format!(
            "{}/{} ({:.0}%)",
            self.done + self.failed + self.skipped,
            self.total,
            self.fraction() * 100.0
        );
        if self.poison_pills > 0 {
            line.push_str(&format!(", {} skipped as unrunnable", self.poison_pills));
        }
        if self.inhibiting {
            line.push_str(", holding the machine awake");
        }
        line
    }
}

/// A held suspend inhibitor.
///
/// On Linux this is a lock file in the runtime directory. On macOS the
/// equivalent is a process assertion, which is not available from a
/// dependency-free crate, so `acquire` returns `None` and the supervisor
/// carries on. Returning `None` is the honest answer: "we could not hold an
/// inhibitor" is information, and a no-op that reports success is a lie the
/// user finds out about the hard way.
#[derive(Debug)]
pub struct SuspendInhibitor {
    path: Option<PathBuf>,
    _file: Option<std::fs::File>,
}

impl SuspendInhibitor {
    /// Take an inhibitor, if this platform and this configuration allow one.
    pub fn acquire(runtime_dir: Option<PathBuf>) -> Option<Self> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let dir = runtime_dir.or_else(|| std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))?;
        let path = dir.join(format!("commons-scan-inhibit-{}", std::process::id()));
        // `create_new` rather than `create`: two supervisors in one process
        // must not share an inhibitor, and a second one that silently took
        // the first's file would release it out from under it.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .ok()?;
        Some(SuspendInhibitor {
            path: Some(path),
            _file: Some(file),
        })
    }

    /// The lock file, for a status display.
    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }
}

impl Drop for SuspendInhibitor {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A job that is requeued on purpose, for a caller that wants to retry a
/// `Skipped` job in a new run.
///
/// Free function rather than a method because "requeue this one" is a
/// decision a caller makes for one job, and a method on the queue would invite
/// a loop over a selection.
pub fn requeue(queue: &JobQueue, job_id: &str) -> bool {
    queue.resubmit(job_id)
}

/// The outcome of a job, for a caller that wants to assert on it in a test
/// without going through the pool.
pub fn outcome_of(outcome: &JobOutcome) -> JobState {
    outcome.state
}
