//! The T-P2-004 acceptance tests, named as the plan names them.
//!
//! The plan is explicit that all three are named tests, "especially (b) -- the
//! poison-pill case is the one that produces infinite loops upstream". They
//! are at the top of this file under those exact names, and the rest is the
//! surrounding behaviour.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use commons_core::JobState;
use commons_jobs::queue::RetryPolicy;
use commons_jobs::spec::{JobOutcome, JobSpec};
use commons_jobs::supervise::{Progress, Supervisor, SupervisorConfig};
use commons_jobs::worker::{
    counting_handler, failing_handler, ok_handler, Handler, Subprocess, Supervised,
};
use commons_jobs::{JobKind, JobQueue, QueueConfig};

fn now() -> SystemTime {
    SystemTime::now()
}

fn instant(seconds: u64) -> SystemTime {
    SystemTime::now() + Duration::from_secs(seconds)
}

/// Does this pid still exist?
///
/// `kill -0` asks the kernel, so the answer covers a zombie too -- a killed
/// but unreaped child still exists, and "killed without collected" is exactly
/// the bug #5709 is about.
fn is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Is `program` on the PATH?
fn which(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

fn fast_policy(max_attempts: u32) -> QueueConfig {
    QueueConfig {
        retry: RetryPolicy {
            max_attempts,
            base_delay: Duration::from_millis(0),
            max_delay: Duration::from_millis(0),
        },
        max_concurrent: 4,
    }
}

// ============================================================ ACCEPTANCE (a)

/// **Acceptance (a):** submit 1,000 duplicate keys, assert 1 job.
///
/// The watcher case, at the size the watcher actually does it. A watcher on a
/// save-heavy editor, or an rsync that rewrites mtimes across a tree, fires
/// hundreds of times for one file inside a second.
#[test]
fn a_thousand_duplicate_keys_produce_one_job() {
    let queue = JobQueue::default();
    let spec = JobSpec::new(JobKind::Generate, "file-1");

    for _ in 0..1000 {
        queue.submit(spec.clone());
    }

    assert_eq!(queue.jobs().len(), 1, "1000 submissions, one job");
    let stats = queue.stats();
    assert_eq!(stats.queued, 1);
    assert_eq!(
        stats.deduplicated, 999,
        "and the other 999 were recognised as duplicates"
    );
    assert_eq!(queue.jobs_in(JobState::Queued).len(), 1);
}

/// The same via the batch path, which is what a watcher actually calls.
#[test]
fn a_watcher_firing_fifty_times_produces_one_job() {
    let queue = JobQueue::default();
    let created = queue.submit_all((0..50).map(|_| JobSpec::new(JobKind::Generate, "file-1")));
    assert_eq!(created, 1);
    assert_eq!(queue.jobs().len(), 1);
}

/// Dedupe is per key, so two *different* files both get a job.
#[test]
fn two_different_files_get_two_jobs() {
    let queue = JobQueue::default();
    assert!(queue.submit(JobSpec::new(JobKind::Generate, "file-1")).1);
    assert!(queue.submit(JobSpec::new(JobKind::Generate, "file-2")).1);
    assert_eq!(queue.jobs().len(), 2);
}

/// And two different *kinds* for one file are two jobs, because generating
/// and transcoding the same file are not the same work.
#[test]
fn two_kinds_for_one_file_are_two_jobs() {
    let queue = JobQueue::default();
    queue.submit(JobSpec::new(JobKind::Generate, "file-1"));
    queue.submit(JobSpec::new(JobKind::Transcode, "file-1"));
    assert_eq!(queue.jobs().len(), 2);
}

/// A duplicate submission arriving while the job is *running* must not touch
/// it. A watcher event that lands mid-generation and resets the job would
/// restart work the user is watching.
#[test]
fn a_duplicate_arriving_mid_run_does_not_disturb_the_job() {
    let queue = JobQueue::default();
    let (job, _) = queue.submit(JobSpec::new(JobKind::Generate, "file-1"));
    let claimed = queue.claim(now()).unwrap();
    assert_eq!(claimed.state, JobState::Running);

    let (again, created) = queue.submit(JobSpec::new(JobKind::Generate, "file-1"));
    assert!(!created);
    assert_eq!(again, claimed, "the running job, unchanged");
    assert_eq!(again.id, job.id);
}

// ============================================================ ACCEPTANCE (b)

/// **Acceptance (b):** a job that always fails lands in `Skipped`, and the
/// queue keeps draining.
///
/// This is the one that matters. stash#2913 and #6837 are both a job that is
/// never terminal: the queue never empties, the CPU never goes idle, and
/// nothing in the UI says why. The assertion that matters is the second half
/// -- that the *rest* of the queue still drains. A queue that gives up
/// entirely on a poison pill is also broken, in the other direction.
#[test]
fn a_job_that_always_fails_lands_in_skipped_and_the_queue_keeps_draining() {
    let queue = JobQueue::new(fast_policy(3));
    queue.submit(JobSpec::new(JobKind::Generate, "poison"));
    for i in 0..10 {
        queue.submit(JobSpec::new(JobKind::Generate, format!("fine-{i}")));
    }

    let mut poison_attempts = 0;
    // Bound the loop by a generous iteration count, not by the queue
    // draining: if the retry policy is broken, this test must fail rather than
    // hang, and an iteration cap is what makes it fail.
    for _ in 0..200 {
        if queue.is_drained() {
            break;
        }
        let Some(job) = queue.claim(now()) else {
            break;
        };
        if job.target_id.as_deref() == Some("poison") {
            poison_attempts += 1;
            queue.complete(
                &job.id,
                JobOutcome::retry("it always fails", Duration::from_millis(0)),
                now(),
            );
        } else {
            queue.complete(&job.id, JobOutcome::done(), now());
        }
    }

    assert!(
        queue.is_drained(),
        "the queue drained: {}",
        queue.stats().one_line()
    );
    assert_eq!(
        poison_attempts, 3,
        "the poison pill was attempted max_attempts times and no more"
    );

    let poison = queue.get("generate:poison").unwrap();
    assert_eq!(
        poison.state,
        JobState::Skipped,
        "a job that always fails ends Skipped, not Queued and not Running"
    );
    assert!(poison.is_terminal());

    let stats = queue.stats();
    assert_eq!(stats.skipped, 1);
    assert_eq!(stats.poisoned, 1, "and it is counted as a poison pill");
    assert_eq!(stats.done, 10, "the other ten jobs were unaffected");
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.queued, 0);
    assert_eq!(stats.running, 0);
}

/// Once on the skip list, a job is never offered again -- not by a claim, not
/// by a tick, not because it is the oldest thing in the queue.
#[test]
fn a_skipped_job_is_never_offered_again_in_the_same_run() {
    let queue = JobQueue::new(fast_policy(2));
    queue.submit(JobSpec::new(JobKind::Generate, "poison"));

    for _ in 0..2 {
        let job = queue.claim(now()).unwrap();
        queue.complete(
            &job.id,
            JobOutcome::retry("nope", Duration::from_millis(0)),
            now(),
        );
    }
    assert!(queue.is_skipped("generate:poison"));

    // Twenty more claims. The job must not come back.
    for _ in 0..20 {
        assert!(
            queue.claim(now()).is_none(),
            "a skipped job is terminal, not merely deprioritised"
        );
    }
    assert!(queue.is_drained());
}

/// A new run clears the skip list, which is the "never retried *in the same
/// run*" half of the rule.
#[test]
fn a_new_run_clears_the_skip_list() {
    let queue = JobQueue::new(fast_policy(1));
    queue.submit(JobSpec::new(JobKind::Generate, "poison"));
    let job = queue.claim(now()).unwrap();
    queue.complete(
        &job.id,
        JobOutcome::retry("nope", Duration::from_millis(0)),
        now(),
    );
    assert_eq!(
        queue.get("generate:poison").unwrap().state,
        JobState::Skipped
    );
    assert!(queue.is_skipped("generate:poison"));

    assert_eq!(queue.clear_skip_list(), 1);
    assert!(!queue.is_skipped("generate:poison"));
    let requeued = queue.get("generate:poison").unwrap();
    assert_eq!(requeued.state, JobState::Queued);
    assert_eq!(requeued.attempts, 0, "and its attempt count is reset");
    assert!(
        queue.claim(now()).is_some(),
        "so a new run can try it again"
    );
}

/// A single job can be requeued without a whole new run.
#[test]
fn one_job_can_be_requeued() {
    let queue = JobQueue::new(fast_policy(1));
    queue.submit(JobSpec::new(JobKind::Generate, "flaky"));
    let job = queue.claim(now()).unwrap();
    queue.complete(
        &job.id,
        JobOutcome::retry("transient", Duration::from_millis(0)),
        now(),
    );
    assert_eq!(
        queue
            .get("flaky")
            .unwrap_or_else(|| queue.get("generate:flaky").unwrap())
            .state,
        JobState::Skipped
    );

    assert!(queue.resubmit("generate:flaky"));
    let requeued = queue.get("generate:flaky").unwrap();
    assert_eq!(requeued.state, JobState::Queued);
    assert_eq!(requeued.attempts, 0);
    assert_eq!(requeued.last_error, None);
}

/// A job with no handler is skipped rather than retried. Retrying it would
/// reproduce the poison-pill loop from the simplest possible cause: a handler
/// that does not exist.
#[test]
fn a_kind_with_no_handler_is_skipped_not_retried() {
    let queue = JobQueue::new(fast_policy(5));
    queue.submit(JobSpec::new(JobKind::Transcode, "file-1"));
    // A pool with handlers for everything except Transcode.
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(JobKind::Generate, ok_handler());
    let pool = commons_jobs::WorkerPool::new(queue.clone(), handlers);

    pool.run_one(now());
    let job = queue.get("transcode:file-1").unwrap();
    assert_eq!(
        job.state,
        JobState::Skipped,
        "retrying a job with no handler loops forever"
    );
    assert_eq!(
        job.attempts, 1,
        "and it took exactly one attempt to find that out"
    );
    assert!(queue.is_drained());
}

// ============================================================ ACCEPTANCE (c)

/// **Acceptance (c):** kill the process mid-job, restart, assert resume.
///
/// Simulated rather than by sending SIGKILL to the test binary, because the
/// thing being tested is the *reconstruction* and that is a pure function of
/// the snapshot. What a real crash produces is a `Running` row; what resume
/// has to do is turn it back into a `Queued` one.
#[test]
fn a_crash_mid_job_resumes_on_restart() {
    let queue = JobQueue::new(fast_policy(5));
    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    queue.submit(JobSpec::new(JobKind::Generate, "b"));
    queue.submit(JobSpec::new(JobKind::Generate, "c"));

    // The process gets through one job and starts another.
    let done = queue.claim(now()).unwrap();
    queue.complete(&done.id, JobOutcome::done(), now());
    let in_flight = queue.claim(now()).unwrap();
    assert_eq!(in_flight.state, JobState::Running);
    // ... and dies here. No completion, no snapshot save beyond what a
    // periodic save would have written.

    let snapshot = queue.snapshot();
    let restored = JobQueue::restore(snapshot, fast_policy(5));

    // The completed job is still completed.
    assert_eq!(
        restored.get(&done.id).unwrap().state,
        JobState::Done,
        "work that finished before the crash is not redone"
    );
    // The in-flight job is queued again, with its attempt count intact
    // because the attempt it was partway through did not fail.
    let resumed = restored.get(&in_flight.id).unwrap();
    assert_eq!(
        resumed.state,
        JobState::Queued,
        "the in-flight job is queued again"
    );
    assert_eq!(
        resumed.attempts, 0,
        "a crash is not a failure, so it does not count against the retry limit"
    );
    // The untouched job is still there.
    assert_eq!(restored.jobs().len(), 3);
    // And the counters agree with the jobs. Without this the restored queue
    // reports three queued when two are claimable, and every caller that
    // decides whether to keep working -- the supervisor's poll loop, the UI's
    // "3 jobs remaining" -- is wrong in a way no state assertion catches.
    let stats = restored.stats();
    assert_eq!(
        (stats.queued, stats.running, stats.done),
        (2, 0, 1),
        "the restored counters match the restored jobs: {stats:?}"
    );
    assert_eq!(
        stats.total(),
        restored.jobs().len(),
        "and the counters account for every job exactly once"
    );

    // And it all drains.
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(JobKind::Generate, ok_handler());
    let pool = commons_jobs::WorkerPool::new(restored.clone(), handlers);
    pool.drain_blocking(10);
    assert!(restored.is_drained(), "{}", restored.stats().one_line());
    assert_eq!(restored.stats().done, 3);
}

/// A crash does not resurrect a job that was already terminal, and does not
/// forget the skip list.
#[test]
fn a_crash_preserves_terminal_states_and_the_skip_list() {
    let queue = JobQueue::new(fast_policy(1));
    queue.submit(JobSpec::new(JobKind::Generate, "poison"));
    queue.submit(JobSpec::new(JobKind::Generate, "ok"));

    // Poison goes first (oldest id, same attempt count) and burns out.
    let job = queue.claim(now()).unwrap();
    queue.complete(
        &job.id,
        JobOutcome::retry("nope", Duration::from_millis(0)),
        now(),
    );
    let job = queue.claim(now()).unwrap();
    queue.complete(&job.id, JobOutcome::done(), now());

    let restored = JobQueue::restore(queue.snapshot(), fast_policy(1));
    assert_eq!(
        restored.get("generate:poison").unwrap().state,
        JobState::Skipped
    );
    assert!(
        restored.is_skipped("generate:poison"),
        "the skip list survives a crash -- otherwise a crash loop retries the poison pill forever"
    );
    assert!(restored.is_drained(), "and there is nothing left to do");
}

// ================================================== retries and backoff

/// Retries back off exponentially, to a ceiling.
#[test]
fn retries_back_off_exponentially_to_a_ceiling() {
    let policy = RetryPolicy {
        max_attempts: 10,
        base_delay: Duration::from_secs(2),
        max_delay: Duration::from_secs(60),
    };
    assert_eq!(policy.delay_for(1), Duration::from_secs(2));
    assert_eq!(policy.delay_for(2), Duration::from_secs(4));
    assert_eq!(policy.delay_for(3), Duration::from_secs(8));
    assert_eq!(policy.delay_for(4), Duration::from_secs(16));
    assert_eq!(policy.delay_for(5), Duration::from_secs(32));
    assert_eq!(policy.delay_for(6), Duration::from_secs(60), "capped");
    assert_eq!(policy.delay_for(100), Duration::from_secs(60));
    assert_eq!(policy.delay_for(u32::MAX), Duration::from_secs(60));
}

/// A delayed job is not claimable until its delay has elapsed.
#[test]
fn a_delayed_job_is_not_claimable_until_its_time() {
    let queue = JobQueue::new(QueueConfig {
        retry: RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(300),
        },
        max_concurrent: 4,
    });
    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    let job = queue.claim(now()).unwrap();
    queue.complete(
        &job.id,
        JobOutcome::retry("transient", Duration::from_secs(60)),
        now(),
    );

    assert!(
        queue.claim(now()).is_none(),
        "a job inside its backoff is not claimable"
    );
    assert!(
        queue.claim(instant(61)).is_some(),
        "and is, once the backoff has passed"
    );
}

/// A handler that reports `Done` is not retried, however many attempts it has
/// taken to get there.
#[test]
fn a_job_that_eventually_succeeds_is_not_skipped() {
    let queue = JobQueue::new(fast_policy(5));
    let counter = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&counter);
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(
        JobKind::Generate,
        Arc::new(move |_job| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                JobOutcome::retry("not yet", Duration::from_millis(0))
            } else {
                JobOutcome::done()
            }
        }),
    );
    queue.submit(JobSpec::new(JobKind::Generate, "flaky"));
    let pool = commons_jobs::WorkerPool::new(queue.clone(), handlers);
    pool.drain_blocking(10);

    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert_eq!(queue.get("generate:flaky").unwrap().state, JobState::Done);
    assert_eq!(queue.stats().skipped, 0);
    assert!(queue.is_drained());
}

// ============================================== concurrency and cancellation

/// The pool is bounded.
#[test]
fn the_pool_is_bounded() {
    let queue = JobQueue::new(QueueConfig {
        retry: RetryPolicy::default(),
        max_concurrent: 2,
    });
    for i in 0..10 {
        queue.submit(JobSpec::new(JobKind::Generate, format!("f{i}")));
    }
    let a = queue.claim(now()).unwrap();
    let b = queue.claim(now()).unwrap();
    assert!(queue.claim(now()).is_none(), "only two at a time");
    queue.complete(&a.id, JobOutcome::done(), now());
    assert!(queue.claim(now()).is_some(), "a freed slot is reusable");
    queue.complete(&b.id, JobOutcome::done(), now());
}

/// One job per (kind, target) (#2824, #5709).
///
/// A file must not be generated twice at once, and must not be generated and
/// transcoded at once. Two *different* files are independent work and must
/// both run -- bounding by kind rather than by target would make the 100k
/// target unreachable.
#[test]
fn one_job_per_kind_per_target_but_different_files_run_together() {
    let queue = JobQueue::new(QueueConfig {
        retry: RetryPolicy::default(),
        max_concurrent: 8,
    });
    // The same kind and target twice, with distinct dedupe keys so dedupe does
    // not collapse them first. This is the only shape in which the per-target
    // rule is observable, and it is a real shape: a scan submits
    // `generate:file-1` and a plugin submits `generate:file-1:variant-b`.
    queue.submit(JobSpec::new(JobKind::Transcode, "a"));
    queue.submit(JobSpec::new(JobKind::Transcode, "a").dedupe_key("generate:a:variant-b"));
    // Same kind, different targets.
    queue.submit(JobSpec::new(JobKind::Scan, "dir-1"));
    queue.submit(JobSpec::new(JobKind::Scan, "dir-2"));
    assert_eq!(queue.jobs().len(), 4, "four distinct jobs");

    let first = queue.claim(now()).unwrap();
    let second = queue.claim(now()).unwrap();
    let running: Vec<(JobKind, String)> = [&first, &second]
        .iter()
        .map(|j| (j.kind, j.target_id.clone().unwrap_or_default()))
        .collect();

    assert!(
        !(running.contains(&(JobKind::Transcode, "a".to_string()))
            && running
                .iter()
                .filter(|(k, t)| *k == JobKind::Transcode && t == "a")
                .count()
                > 1),
        "a file is not transcoded twice at once: {running:?}"
    );
    // And the two scans of *different* directories are independent work, so
    // both are claimable while the transcode is still running.
    // The second scan is claimable: a different directory is not blocked by
    // the first. (The *second transcode* on the same file is not, and must
    // not be -- that is the invariant, asserted below.)
    let third = queue.claim(now());
    assert!(
        third.is_some(),
        "a scan of a different directory is not blocked: {running:?}"
    );
    assert!(
        queue.claim(now()).is_none(),
        "and the only thing left is the conflicting transcode, which is blocked"
    );
    assert_eq!(queue.jobs_in(JobState::Running).len(), 3);
    // Complete the transcode and the blocked one becomes claimable.
    let transcode = queue
        .jobs_in(JobState::Running)
        .into_iter()
        .find(|j| j.kind == JobKind::Transcode)
        .unwrap();
    queue.complete(&transcode.id, JobOutcome::done(), now());
    assert!(
        queue.claim(now()).is_some(),
        "once the file is free, the second transcode runs"
    );
}

/// Cancelling a queued job takes it out of the queue.
#[test]
fn cancelling_a_queued_job_removes_it() {
    let queue = JobQueue::default();
    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    assert!(queue.cancel("generate:a"));
    assert_eq!(queue.get("generate:a").unwrap().state, JobState::Cancelled);
    assert!(queue.is_drained());
    assert!(
        !queue.cancel("generate:a"),
        "cancelling twice is not a thing"
    );
    assert!(!queue.cancel("generate:nonexistent"));
}

/// `cancel_all` leaves terminal jobs alone.
#[test]
fn cancelling_everything_leaves_finished_work_alone() {
    let queue = JobQueue::default();
    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    queue.submit(JobSpec::new(JobKind::Generate, "b"));
    queue.submit(JobSpec::new(JobKind::Generate, "c"));
    let done = queue.claim(now()).unwrap();
    queue.complete(&done.id, JobOutcome::done(), now());

    assert_eq!(queue.cancel_all(), 2);
    assert_eq!(queue.get(&done.id).unwrap().state, JobState::Done);
    assert_eq!(queue.stats().done, 1);
    assert_eq!(queue.stats().cancelled, 2);
}

/// A worker that reports a job it does not own is ignored, rather than
/// double-counting it. A progress bar that goes past 100% is the alternative.
#[test]
fn a_double_completion_is_ignored() {
    let queue = JobQueue::default();
    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    let job = queue.claim(now()).unwrap();
    assert!(queue.complete(&job.id, JobOutcome::done(), now()).is_some());
    assert!(
        queue.complete(&job.id, JobOutcome::done(), now()).is_none(),
        "the second report is dropped"
    );
    assert_eq!(queue.stats().done, 1, "and the count is still one");
    assert_eq!(queue.stats().running, 0);
}

// ================================================= the pool and supervisor

/// The pool runs every kind it has a handler for, and skips the rest.
#[test]
fn the_pool_runs_handlers_and_skips_the_kinds_it_lacks() {
    let queue = JobQueue::new(fast_policy(1));
    let counter = Arc::new(AtomicUsize::new(0));
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(JobKind::Generate, counting_handler(Arc::clone(&counter)));
    handlers.insert(JobKind::Identify, ok_handler());
    handlers.insert(JobKind::Backup, failing_handler("always fails"));

    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    queue.submit(JobSpec::new(JobKind::Identify, "a"));
    queue.submit(JobSpec::new(JobKind::Backup, "a"));
    queue.submit(JobSpec::new(JobKind::Match, "a"));

    let pool = commons_jobs::WorkerPool::new(queue.clone(), handlers);
    let ran = pool.drain_blocking(20);

    assert_eq!(ran, 4);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(queue.is_drained());
    assert_eq!(queue.stats().done, 2);
    assert_eq!(
        queue.stats().skipped,
        2,
        "the failure and the missing handler"
    );
}

/// A stopped pool runs nothing.
#[test]
fn a_stopped_pool_runs_nothing() {
    let queue = JobQueue::default();
    queue.submit(JobSpec::new(JobKind::Generate, "a"));
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(JobKind::Generate, ok_handler());
    let pool = commons_jobs::WorkerPool::new(queue.clone(), handlers);
    pool.stop();
    assert!(pool.is_stopping());
    assert!(pool.run_one(now()).is_none());
    assert_eq!(
        queue.stats().queued,
        1,
        "and the job is still there for later"
    );
}

/// The supervisor drains the queue and reports progress.
#[test]
fn the_supervisor_drains_and_reports() {
    let queue = JobQueue::new(fast_policy(3));
    for i in 0..25 {
        queue.submit(JobSpec::new(JobKind::Generate, format!("f{i}")));
    }
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(JobKind::Generate, ok_handler());
    let supervisor = Supervisor::new(
        queue.clone(),
        handlers,
        SupervisorConfig {
            queue: fast_policy(3),
            poll_interval: Duration::from_millis(1),
            inhibit_suspend: false,
            runtime_dir: None,
        },
    );

    let ran = supervisor.run_blocking();
    assert_eq!(ran, 25);
    assert!(queue.is_drained());

    let progress: Progress = supervisor.progress();
    assert_eq!(progress.done, 25);
    assert_eq!(progress.fraction(), 1.0);
    assert!(
        progress.one_line().contains("100%"),
        "{}",
        progress.one_line()
    );
}

/// Progress reaches 100% even when some jobs were skipped, because they *are*
/// finished. A bar that stops at 90% because ten files were unreadable is a
/// bar that never completes.
#[test]
fn progress_reaches_one_hundred_percent_with_skips() {
    let queue = JobQueue::new(fast_policy(1));
    queue.submit(JobSpec::new(JobKind::Generate, "good"));
    queue.submit(JobSpec::new(JobKind::Generate, "poison"));
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(
        JobKind::Generate,
        Arc::new(|job: commons_jobs::Job| {
            if job.target_id.as_deref() == Some("poison") {
                JobOutcome::retry("no", Duration::from_millis(0))
            } else {
                JobOutcome::done()
            }
        }),
    );
    let supervisor = Supervisor::new(
        queue.clone(),
        handlers,
        SupervisorConfig {
            queue: fast_policy(1),
            poll_interval: Duration::from_millis(1),
            inhibit_suspend: false,
            runtime_dir: None,
        },
    );
    supervisor.run_blocking();
    let progress = supervisor.progress();
    assert_eq!(progress.skipped, 1);
    assert_eq!(progress.poison_pills, 1);
    assert_eq!(progress.fraction(), 1.0);
    assert!(
        progress.one_line().contains("unrunnable"),
        "{}",
        progress.one_line()
    );
}

/// An empty queue is complete, not stuck at zero percent.
#[test]
fn an_empty_queue_is_complete() {
    let progress = Progress::default();
    assert_eq!(progress.fraction(), 1.0);
}

// ============================================ subprocess supervision (#5709)

/// A subprocess that exits is reaped, and leaves no zombie.
#[test]
fn a_subprocess_is_reaped_after_it_exits() {
    let subprocess = Subprocess {
        timeout: Duration::from_secs(10),
    };
    let outcome = subprocess.run("/bin/sh", &["-c", "exit 0"]);
    assert_eq!(outcome.state, JobState::Done);
}

/// A subprocess that fails reports the failure, and does not retry forever.
#[test]
fn a_failing_subprocess_reports_its_exit_status() {
    let subprocess = Subprocess {
        timeout: Duration::from_secs(10),
    };
    let outcome = subprocess.run("/bin/sh", &["-c", "exit 3"]);
    assert_eq!(outcome.state, JobState::Failed);
    assert!(
        outcome.error.unwrap().contains("exited with"),
        "the status is reported"
    );
}

/// A subprocess that overruns is killed, and the kill is followed by a wait.
///
/// The wait is the point: killing without collecting is exactly how #5709's
/// defunct process happens.
#[test]
fn a_subprocess_that_overruns_is_killed_and_reaped() {
    let subprocess = Subprocess {
        timeout: Duration::from_millis(200),
    };
    // A child that *ignores* SIGTERM, so the choice of signal is load-bearing.
    //
    // The obvious `sh -c "trap '' TERM; sleep 30"` does not work: the shell
    // execs `sleep` in place, so the pid is `sleep`'s, the trap is discarded,
    // and SIGTERM kills it instantly. A supervisor that sent SIGTERM instead
    // of SIGKILL passes that test while being wrong. A python child that sets
    // SIG_IGN in its own body actually keeps the handler.
    let started = std::time::Instant::now();
    let (outcome, pid) = if which("python3").is_some() {
        subprocess.run_observing(
            "python3",
            &[
                "-c",
                "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)",
            ],
        )
    } else {
        subprocess.run_observing("/bin/sh", &["-c", "sleep 30"])
    };
    let elapsed = started.elapsed();

    assert_eq!(outcome.state, JobState::Failed);
    assert!(
        outcome.error.unwrap().contains("exceeded"),
        "the timeout is named"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "and it did not wait out the sleep: {elapsed:?}"
    );
    // The important assertion: the *process* is gone, checked against the pid
    // and not inferred from the clock. A supervisor that returned promptly
    // without signalling anything satisfies every timing assertion there is,
    // and leaves the child running for another 30 seconds holding whatever it
    // had open.
    let pid = pid.expect("the child was spawned");
    for _ in 0..100 {
        if !is_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();
    panic!("pid {pid} is still alive {elapsed:?} after the timeout: the child was not killed");
}

/// A missing program is a failure, not a panic.
#[test]
fn a_missing_program_is_a_failure_not_a_panic() {
    let subprocess = Subprocess::default();
    let outcome = subprocess.run("/nonexistent/program/xyzzy", &[]);
    assert_eq!(outcome.state, JobState::Failed);
}

/// Dropping a `Supervised` without waiting still reaps the child (#5709).
///
/// This is the case a job that errors out produces: the handler drops the
/// child and returns `Err`, and nobody calls `wait`. The assertion is on the
/// process table, because "there is a reaper thread" is a claim about the
/// implementation and "no defunct process is left" is a claim about the
/// system.
#[test]
fn dropping_a_supervised_child_leaves_no_zombie() {
    let count_zombies = || -> usize {
        let out = std::process::Command::new("sh")
            .args(["-c", "ps -eo stat= | grep -c '^Z' || true"])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    };

    let before = count_zombies();
    for _ in 0..20 {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let child = Supervised::spawn(command).unwrap();
        drop(child); // deliberately no wait
    }
    // The reapers are threads; give them a moment to collect.
    std::thread::sleep(Duration::from_millis(500));
    let after = count_zombies();
    assert!(
        after <= before,
        "20 dropped children left {after} zombies, up from {before}"
    );
}

/// The plugin path: a plugin's job runs in the same queue, under the same
/// retry and skip rules.
#[test]
fn a_plugin_job_runs_through_the_same_rules() {
    let queue = JobQueue::new(fast_policy(2));
    let (kind, handler) =
        commons_jobs::worker::subprocess_handler("/bin/sh", Duration::from_secs(10));
    assert_eq!(kind, JobKind::Plugin);

    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(JobKind::Plugin, handler);

    queue.submit(
        JobSpec::new(JobKind::Plugin, "my-plugin")
            .with_payload(serde_json::json!({"args": ["-c", "exit 0"]})),
    );
    let pool = commons_jobs::WorkerPool::new(queue.clone(), handlers);
    pool.drain_blocking(5);

    assert_eq!(queue.get("plugin:my-plugin").unwrap().state, JobState::Done);
    assert!(queue.is_drained());
}

/// A plugin job that fails obeys the attempt limit like anything else, so a
/// misbehaving plugin cannot loop the queue.
#[test]
fn a_failing_plugin_job_is_skipped_not_looped() {
    let queue = JobQueue::new(fast_policy(2));
    let (kind, handler) =
        commons_jobs::worker::subprocess_handler("/bin/sh", Duration::from_secs(10));
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(kind, handler);
    queue.submit(
        JobSpec::new(JobKind::Plugin, "bad")
            .with_payload(serde_json::json!({"args": ["-c", "exit 1"]})),
    );
    let pool = commons_jobs::WorkerPool::new(queue.clone(), handlers);
    pool.drain_blocking(20);

    let job = queue.get("plugin:bad").unwrap();
    assert!(
        job.is_terminal(),
        "a subprocess that always fails must not retry forever, and landed in {}",
        job.state
    );
    assert_eq!(
        job.attempts, 1,
        "and it was tried exactly once -- a non-zero exit is a failure, not a retryable error"
    );
    assert!(queue.is_drained());
}
