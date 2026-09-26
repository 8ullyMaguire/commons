//! T-P2-004's two remaining properties: inhibit-suspend (#5517) and plugins
//! submitting into the same queue (#5944).
//!
//! Both are things a supervisor *claims* to do. These check it did.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use commons_jobs::spec::JobSpec;
use commons_jobs::supervise::{Supervisor, SupervisorConfig};
use commons_jobs::worker::{subprocess_handler, Handler};
use commons_jobs::{JobKind, JobOutcome, JobQueue, QueueConfig};

fn config() -> QueueConfig {
    QueueConfig {
        retry: commons_jobs::queue::RetryPolicy {
            max_attempts: 2,
            base_delay: std::time::Duration::from_millis(0),
            max_delay: std::time::Duration::from_millis(0),
        },
        max_concurrent: 2,
    }
}

// ------------------------------------------------------------------ #5517

/// While the supervisor has work *in flight*, the lock file exists; when the
/// work is done, it is gone.
///
/// The file is the whole mechanism on Linux without a libsystemd dependency:
/// `systemd-inhibit` is looked up on the PATH by the agent and its presence is
/// exactly this file's existence. Asserting on the filesystem rather than on
/// a return value is the point -- "acquire returned true" is a claim about the
/// code, "the file is there" is a claim about the machine.
///
/// A handler that blocks on a channel keeps the job running, so the assertion
/// happens while the work is genuinely in flight. The first version of this
/// test let the job finish inside the same tick, so the inhibitor was
/// legitimately released before it looked, and removing the acquire entirely
/// passed.
#[test]
fn the_inhibitor_exists_while_there_is_work_and_not_after() {
    let runtime = tempfile::tempdir().unwrap();
    // A gate rather than a channel: a `Handler` is `Sync`, and a
    // `mpsc::Receiver` is not.
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (gate, seen) = (
        std::sync::Arc::clone(&release),
        std::sync::Arc::clone(&entered),
    );
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(
        JobKind::Scan,
        std::sync::Arc::new(move |_job| {
            seen.wait();
            while !gate.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            JobOutcome::done()
        }),
    );

    let queue = JobQueue::new(config());
    let supervisor = Supervisor::new(
        queue.clone(),
        handlers,
        SupervisorConfig {
            runtime_dir: Some(PathBuf::from(runtime.path())),
            ..SupervisorConfig::default()
        },
    );
    let inhibitor = runtime
        .path()
        .join(format!("commons-scan-inhibit-{}", std::process::id()));
    assert!(!inhibitor.exists(), "nothing to inhibit before work starts");

    queue.submit(JobSpec::new(JobKind::Scan, "library"));
    // A clone, so the supervisor itself is still here to inspect afterwards.
    let ticking = supervisor.clone();
    let worker = std::thread::spawn(move || ticking.tick(SystemTime::now()));
    entered.wait();

    assert!(
        inhibitor.exists(),
        "the lock file is held while a job is in flight: {}",
        inhibitor.display()
    );
    release.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(worker.join().unwrap());

    // One more tick, which is the caller's next loop iteration. The queue is
    // now empty, and this tick must notice that *before* it tries to run
    // anything -- so the inhibitor has to be gone by the time it returns.
    //
    // This uses *this* supervisor, not a fresh one. A fresh supervisor with
    // `inhibit_suspend: false` passes even when the original never released
    // anything, which is exactly the bug: a stale `systemd-inhibit` file left
    // behind for the rest of the session.
    assert!(!supervisor.tick(SystemTime::now()), "nothing left to run");
    assert!(
        !inhibitor.exists(),
        "and the lock file is gone on the next tick: {}",
        inhibitor.display()
    );
    assert!(
        !supervisor.is_inhibiting(),
        "and the supervisor agrees it is not holding one"
    );
}

/// Two inhibitors in one process do not share a file, so one dropping does not
/// release the other's.
#[test]
fn two_inhibitors_do_not_share_a_file() {
    let runtime = tempfile::tempdir().unwrap();
    let first =
        commons_jobs::supervise::SuspendInhibitor::acquire(Some(PathBuf::from(runtime.path())));
    let second =
        commons_jobs::supervise::SuspendInhibitor::acquire(Some(PathBuf::from(runtime.path())));
    match (first, second) {
        (Some(a), Some(b)) => {
            assert_ne!(a.path(), b.path(), "two inhibitors, two files");
        }
        (Some(a), None) => {
            // The second correctly refused, and must not have taken the first
            // one's file -- which is what `create_new` is for.
            assert!(a.path().unwrap().exists(), "the first still holds its file");
        }
        // No XDG runtime dir configured, or the platform is not Linux:
        // nothing to assert.
        (None, _) => {}
    }
}

/// `inhibit_suspend: false` must not create a file even while a job is in
/// flight.
///
/// The check is made *during* the job, not after. The first version looked at
/// the directory after the queue drained, and passed with the flag ignored --
/// because the file had been created and then released, and "nothing left
/// behind" is what a correct implementation does too.
#[test]
fn inhibition_can_be_turned_off() {
    let runtime = tempfile::tempdir().unwrap();
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (g, e) = (
        std::sync::Arc::clone(&gate),
        std::sync::Arc::clone(&entered),
    );
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    handlers.insert(
        JobKind::Scan,
        std::sync::Arc::new(move |_job| {
            e.wait();
            while !g.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            JobOutcome::done()
        }),
    );

    let queue = JobQueue::new(config());
    let supervisor = Supervisor::new(
        queue.clone(),
        handlers,
        SupervisorConfig {
            inhibit_suspend: false,
            runtime_dir: Some(PathBuf::from(runtime.path())),
            ..SupervisorConfig::default()
        },
    );
    queue.submit(JobSpec::new(JobKind::Scan, "library"));
    let ticking = supervisor.clone();
    let worker = std::thread::spawn(move || ticking.tick(SystemTime::now()));
    entered.wait();

    let leftovers: Vec<_> = std::fs::read_dir(runtime.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "inhibition is off, so nothing was created while a job ran: {leftovers:?}"
    );
    assert!(
        !supervisor.is_inhibiting(),
        "and the supervisor is not holding one"
    );

    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    worker.join().unwrap();
}

// ------------------------------------------------------------------ #5944

/// A plugin submits into the same queue and obeys the same rules (#5944).
///
/// "The same queue" is the claim worth testing: a plugin that has its own
/// private work list is a plugin that has its own poison pill, its own retry
/// limit, and its own unbounded pool.
#[test]
fn a_plugin_submits_into_the_same_queue_and_obeys_the_same_rules() {
    let queue = JobQueue::new(config());
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    let (plugin_kind, handler) = subprocess_handler("/bin/sh", std::time::Duration::from_secs(10));
    assert_eq!(plugin_kind, JobKind::Plugin);
    handlers.insert(JobKind::Plugin, handler);

    // A plugin job and a built-in job, side by side.
    queue.submit(JobSpec::new(JobKind::Plugin, "thumbgen"));
    queue.submit(JobSpec::new(JobKind::Scan, "library"));
    assert_eq!(queue.jobs().len(), 2, "one queue, both kinds");

    let supervisor = Supervisor::new(queue.clone(), handlers, SupervisorConfig::default());
    while supervisor.tick(SystemTime::now()) {}
    supervisor.release_inhibitor();

    // The plugin had no handler for Scan, and an unhandled kind is *skipped*,
    // not failed and not retried -- the same treatment a built-in kind with no
    // handler gets.
    let plugin = queue.get("plugin:thumbgen").unwrap();
    let scan = queue.get("scan:library").unwrap();
    assert_eq!(plugin.state.as_str(), "done", "the plugin ran");
    assert_eq!(
        scan.state.as_str(),
        "skipped",
        "and so did the missing handler"
    );
    assert!(queue.is_drained());
}

/// A plugin job that fails obeys the attempt limit (#2913 applies to plugins).
#[test]
fn a_failing_plugin_is_skipped_not_looped() {
    let queue = JobQueue::new(config());
    let mut handlers: HashMap<JobKind, Handler> = HashMap::new();
    let (plugin_kind, handler) = subprocess_handler("/bin/sh", std::time::Duration::from_secs(10));
    handlers.insert(plugin_kind, handler);
    queue.submit(
        JobSpec::new(JobKind::Plugin, "bad").with_payload(serde_json::json!({
            "args": ["-c", "exit 1"]
        })),
    );
    let supervisor = Supervisor::new(queue.clone(), handlers, SupervisorConfig::default());
    while supervisor.tick(SystemTime::now()) {}
    supervisor.release_inhibitor();

    let job = queue.get("plugin:bad").unwrap();
    assert!(job.is_terminal(), "landed in {}", job.state);
    assert_eq!(job.attempts, 1, "a non-zero exit is not retryable");
    assert!(queue.is_drained(), "{}", queue.stats().one_line());
}

/// The supervisor reports progress by kind, and a plugin shows up in it.
#[test]
fn progress_includes_plugin_work() {
    let queue = JobQueue::new(config());
    let supervisor = Supervisor::new(queue, HashMap::new(), SupervisorConfig::default());
    let q = supervisor.queue().clone();
    q.submit(JobSpec::new(JobKind::Plugin, "a"));
    q.submit(JobSpec::new(JobKind::Scan, "b"));
    let progress = supervisor.progress();
    assert_eq!(progress.total, 2);
    assert_eq!(progress.by_kind.get(&JobKind::Plugin), Some(&1));
    assert_eq!(progress.by_kind.get(&JobKind::Scan), Some(&1));
}

/// A cancelled plugin job leaves nothing behind.
#[test]
fn a_cancelled_plugin_job_is_terminal() {
    let queue = JobQueue::new(config());
    queue.submit(JobSpec::new(JobKind::Plugin, "a"));
    assert!(queue.cancel("plugin:a"));
    assert_eq!(queue.get("plugin:a").unwrap().state.as_str(), "cancelled");
    assert!(queue.is_drained());
    assert_eq!(
        queue.complete("plugin:a", JobOutcome::done(), SystemTime::now()),
        None,
        // A cancelled job is terminal; completing it must not un-cancel it.
        "and completing a cancelled job is not allowed to resurrect it"
    );
}
