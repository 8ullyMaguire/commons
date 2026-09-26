//! The bounded worker pool, and the subprocess supervision underneath it.
//!
//! # Why the pool is bounded
//!
//! Two separate bounds, and they are not the same thing.
//!
//! [`WorkerPool`] bounds how many *jobs* run at once. That is the ticket's
//! "bounded worker pool" and it is a correctness property as much as a
//! resource one: one job type per file (stash#2824, #5709) means a file must
//! not be generated and transcoded simultaneously, and the way to guarantee
//! that is to bound concurrency and check.
//!
//! [`Supervised`] bounds how long a *subprocess* may live, and reaps it
//! afterwards. #5709 is literally a defunct Python process, and the cause is
//! always the same: a child was spawned, the parent decided it was finished,
//! and nobody ever called `wait`. A pool that only reaps at shutdown leaves a
//! zombie for every job it has ever run.

use std::collections::HashMap;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::spec::{Job, JobKind, JobOutcome, JobSpec};

/// What a handler does with a job.
///
/// Returns an outcome rather than `Result<(), E>` because the queue needs to
/// distinguish "failed, retry" from "failed, skip" from "succeeded", and a
/// bare error cannot say which. A handler that wants the retry behaviour
/// returns [`JobOutcome::retry`], which is the common case, and the pool
/// applies the attempt limit.
pub type Handler = Arc<dyn Fn(Job) -> JobOutcome + Send + Sync>;

/// Runs jobs from a queue, up to a bound, until told to stop.
#[derive(Clone)]
pub struct WorkerPool {
    queue: crate::queue::JobQueue,
    handlers: Arc<HashMap<JobKind, Handler>>,
    stop: Arc<Mutex<bool>>,
}

impl WorkerPool {
    /// A pool over `queue` with a handler per kind.
    ///
    /// A kind with no handler is not an error at startup: a kind nobody has
    /// implemented yet should not stop the kinds that have. A claimed job with
    /// no handler is completed as `Skipped` rather than retried, because
    /// retrying it would loop forever on a handler that does not exist — which
    /// is the poison-pill failure this crate exists to prevent, reproduced by
    /// the simplest possible cause.
    pub fn new(queue: crate::queue::JobQueue, handlers: HashMap<JobKind, Handler>) -> Self {
        WorkerPool {
            queue,
            handlers: Arc::new(handlers),
            stop: Arc::new(Mutex::new(false)),
        }
    }

    /// The queue this pool drains.
    pub fn queue(&self) -> &crate::queue::JobQueue {
        &self.queue
    }

    /// Ask the pool to stop after the current jobs.
    pub fn stop(&self) {
        *self.stop.lock() = true;
    }

    /// Has the pool been asked to stop?
    pub fn is_stopping(&self) -> bool {
        *self.stop.lock()
    }

    /// Claim and run one job. Returns what happened, or `None` if there was
    /// nothing runnable.
    ///
    /// This is the unit both the supervisor and the tests drive, so a test
    /// that drives this directly exercises the same path a worker takes.
    pub fn run_one(&self, now: std::time::SystemTime) -> Option<JobOutcome> {
        self.run_one_notifying(now, &|_job| {})
    }

    /// Claim and run one job, calling `on_claimed` once the job is in hand and
    /// before it runs.
    ///
    /// That callback is where the suspend inhibitor is taken. It cannot live
    /// in the supervisor's own loop, because `run_one` is synchronous: the
    /// supervisor only regains control *after* the job has finished, so a
    /// check between jobs is a check that never sees a job in flight. The
    /// whole point of an inhibitor is to be held for the duration of the work,
    /// and with a synchronous pool only the code that is about to block can
    /// arrange that.
    pub fn run_one_notifying(
        &self,
        now: std::time::SystemTime,
        on_claimed: &(dyn Fn(&Job) + Sync),
    ) -> Option<JobOutcome> {
        if self.is_stopping() {
            return None;
        }
        let job = self.queue.claim(now)?;
        on_claimed(&job);
        let outcome = self.dispatch(&job);
        self.queue.complete(&job.id, outcome.clone(), now);
        Some(outcome)
    }

    fn dispatch(&self, job: &Job) -> JobOutcome {
        match self.handlers.get(&job.kind) {
            Some(handler) => handler(job.clone()),
            None => JobOutcome::skipped(format!("no handler for {}", job.kind)),
        }
    }

    /// Drain the queue, up to `max_jobs` jobs and `budget` of wall clock,
    /// stopping early when the queue has nothing runnable left.
    ///
    /// Stops early rather than sleeping through a retry delay. A caller that
    /// wants to wait for a retry is waiting on an external event anyway, and
    /// blocking a pool thread on a sleep is how a pool ends up with every
    /// worker asleep and no capacity for work that *could* run now.
    ///
    /// The time budget is not belt-and-braces. A round bound alone is not a
    /// bound: a job whose retries never exhaust stays `queued` forever, and
    /// `drain_blocking(20)` on such a queue runs 20 rounds of a real
    /// subprocess with a real subprocess timeout. Twenty rounds is already
    /// minutes. This is precisely the shape of the bug the attempt limit
    /// exists to prevent, so the drain that is supposed to *observe* it must
    /// not itself hang when the limit is broken -- otherwise the test for
    /// "does not retry forever" is the one test that can retry forever.
    pub fn drain_blocking(&self, max_jobs: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut ran = 0;
        for _ in 0..max_jobs {
            if self.is_stopping() || std::time::Instant::now() >= deadline {
                break;
            }
            let stats = self.queue.stats();
            if stats.queued == 0 {
                break;
            }
            if self.run_one(std::time::SystemTime::now()).is_some() {
                ran += 1;
            } else {
                break;
            }
        }
        ran
    }
}

/// A child process that is reaped when nobody is waiting for it.
///
/// The child handle and the collected status are separate. That split is the
/// whole design: the reaper needs exclusive access to the child for the length
/// of its life, so anything that shares the handle has to share a *lock*, and
/// a caller enforcing a timeout cannot wait on a lock that is held for exactly
/// as long as the thing it is trying to bound.
///
/// The first version kept both behind one mutex and had the timeout poll
/// `wait()`. Every poll blocked on the reaper's lock, so the deadline was
/// never reached and a runaway subprocess ran to completion regardless of the
/// timeout. The reaper still needs the lock -- nothing else may `wait` on the
/// same child -- but no caller has to.
#[derive(Debug, Clone)]
pub struct Supervised {
    /// The pid, read once at spawn and never re-read.
    ///
    /// The child handle itself is deliberately *not* a field here. It belongs
    /// to the reaper thread and is never read again after spawn, because any
    /// read would have to take the lock the reaper holds across a blocking
    /// `wait` -- and a caller trying to enforce a timeout would then block for
    /// exactly as long as the child it is trying to bound. That is the bug
    /// this type was rewritten to fix, and keeping the handle here at all is
    /// how it would come back.
    pid: u32,
    /// The status, once the reaper has collected it. Separate so a reader
    /// never contends with the reaper.
    status: Arc<Mutex<Option<ExitStatus>>>,
    /// Whether the reaper is still working.
    reaped: Arc<AtomicBool>,
}

impl Supervised {
    /// Spawn `command`, with stdout and stderr piped so a chatty subprocess
    /// cannot fill a pipe buffer and deadlock against a parent that never
    /// reads.
    pub fn spawn(mut command: Command) -> std::io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn()?;
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let status = Arc::new(Mutex::new(None));
        let reaped = Arc::new(AtomicBool::new(false));

        let reaper_child = Arc::clone(&child);
        let reaper_status = Arc::clone(&status);
        let reaper_flag = Arc::clone(&reaped);
        std::thread::spawn(move || {
            // `wait` blocks, so this thread sleeps for free while the child
            // runs -- no polling, no busy loop.
            if let Ok(exit) = reaper_child.lock().wait() {
                *reaper_status.lock() = Some(exit);
            }
            reaper_flag.store(true, Ordering::SeqCst);
        });

        Ok(Supervised {
            pid,
            status,
            reaped,
        })
    }

    /// The child's pid, read at spawn and never re-read.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The status, once the child has been reaped.
    pub fn status(&self) -> Option<ExitStatus> {
        *self.status.lock()
    }

    /// Is the child still running?
    pub fn is_running(&self) -> bool {
        !self.reaped.load(Ordering::SeqCst)
    }

    /// Send a signal to the child.
    ///
    /// Never touches the child's lock, so it works while the reaper is blocked
    /// in `wait` -- which is the only time it is ever needed.
    pub fn signal(&self, signal: Signal) -> bool {
        let pid = self.pid();
        // `kill(2)` as a command, because this crate has no libc dependency
        // and one shell-out per timeout is not a hot path.
        std::process::Command::new("kill")
            .arg(format!("-{}", signal.number()))
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Kill the child and wait for the reaper to collect it.
    ///
    /// Killing without collecting is exactly how the zombie in #5709 is made:
    /// `kill` sends a signal and the process still has to be reaped.
    pub fn kill(&self) {
        self.signal(Signal::Kill);
        self.wait();
    }

    /// Block until the child has been reaped. Returns its status.
    pub fn wait(&self) -> Option<ExitStatus> {
        // A bounded spin rather than a condvar: the reaper sets the flag
        // immediately after `wait` returns, so the wait is short and a
        // condvar would cost more than it saves for a process that has
        // already exited.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !self.reaped.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                return self.status();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.status()
    }
}

/// The signals a supervisor sends.
///
/// A closed set rather than an `i32`, because a typo in a signal number is a
/// signal nobody anticipated being sent to a user's media pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Ask nicely. The child gets a chance to clean up.
    Term,
    /// No chance. For a child that has already overrun its timeout.
    Kill,
}

impl Signal {
    /// The number passed to `kill`.
    pub const fn number(self) -> i32 {
        match self {
            Signal::Term => 15,
            Signal::Kill => 9,
        }
    }
}

/// A job that runs a subprocess, with a timeout and a reaped child.
#[derive(Debug, Clone)]
pub struct Subprocess {
    /// How long the child may run before it is killed.
    pub timeout: Duration,
}

impl Default for Subprocess {
    fn default() -> Self {
        Subprocess {
            timeout: Duration::from_secs(30 * 60),
        }
    }
}

impl Subprocess {
    /// Run `program` with `args`, and describe how it went.
    ///
    /// The timeout is enforced without ever blocking on the reaper: the loop
    /// reads a flag the reaper sets, so a caller that has given a child 200ms
    /// is not stuck behind a mutex for the child's whole life.
    pub fn run(&self, program: &str, args: &[&str]) -> JobOutcome {
        self.run_observing(program, args).0
    }

    /// As [`Subprocess::run`], and also report the child's pid.
    ///
    /// The pid is what lets a caller check the *process* is gone rather than
    /// infer it from how long the call took. A test that only watches the
    /// clock cannot tell a supervisor that killed the child from one that
    /// forgot to, as long as something else made the call return quickly.
    pub fn run_observing(&self, program: &str, args: &[&str]) -> (JobOutcome, Option<u32>) {
        let child = {
            let mut c = Command::new(program);
            c.args(args);
            match Supervised::spawn(c) {
                Ok(c) => c,
                Err(e) => return (JobOutcome::failed(format!("{program}: {e}")), None),
            }
        };
        let pid = Some(child.pid());

        let deadline = Instant::now() + self.timeout;
        loop {
            if !child.is_running() {
                return (
                    match child.status() {
                        Some(status) if status.success() => JobOutcome::done(),
                        Some(status) => {
                            JobOutcome::failed(format!("{program} exited with {status}"))
                        }
                        // Reaped without a status: killed, or never ours.
                        None => JobOutcome::failed(format!("{program} was killed")),
                    },
                    pid,
                );
            }
            if Instant::now() >= deadline {
                // SIGKILL, not SIGTERM. A child that has already overrun a
                // generous timeout is not going to respond politely, and a
                // half-dead process holding a file handle is worse than a
                // dead one. Then wait, so the reaper has collected it and no
                // zombie is left -- which is the #5709 requirement.
                child.signal(Signal::Kill);
                child.wait();
                return (
                    JobOutcome::failed(format!("{program} exceeded {:?}", self.timeout)),
                    pid,
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A handler that runs a subprocess, for a plugin or an external tool.
///
/// The ticket wants plugins submitting into the same queue, and this is the
/// shape such work takes: a `JobKind::Plugin` job whose payload names a
/// program. It is a constructor rather than a free function so a caller does
/// not have to remember the `Arc` dance to get a `Handler`.
pub fn subprocess_handler(program: impl Into<String>, timeout: Duration) -> (JobKind, Handler) {
    let program = program.into();
    let subprocess = Subprocess { timeout };
    (
        JobKind::Plugin,
        Arc::new(move |job: Job| {
            let args: Vec<String> = job
                .payload
                .as_ref()
                .and_then(|p| p.get("args"))
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
            subprocess.run(&program, &borrowed)
        }),
    )
}

/// A handler that always fails, for the poison-pill tests and for a `--demo`
/// mode.
pub fn failing_handler(error: &'static str) -> Handler {
    Arc::new(move |_job: Job| JobOutcome::retry(error, Duration::from_millis(0)))
}

/// A handler that always succeeds.
pub fn ok_handler() -> Handler {
    Arc::new(|_job: Job| JobOutcome::done())
}

/// A handler that counts what it was given, and succeeds.
///
/// For the tests that need to assert a job actually ran rather than inferring
/// it from the queue's counters.
pub fn counting_handler(counter: Arc<std::sync::atomic::AtomicUsize>) -> Handler {
    Arc::new(move |_job: Job| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        JobOutcome::done()
    })
}

/// Submit a spec into a queue and return whether it was new. A convenience
/// for the many call sites that only care about "was this new".
pub fn submit(queue: &crate::queue::JobQueue, spec: JobSpec) -> bool {
    queue.submit(spec).1
}
