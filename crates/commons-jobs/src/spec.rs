//! What a job is, and what it is for.
//!
//! The queue in `queue.rs` is generic over "a unit of slow work"; this is that
//! unit. `JobKind` is the closed set from the ticket, and `JobSpec` is one
//! unit with enough identity to be deduplicated, retried, and audited.

use std::fmt;

use commons_core::JobState;
use serde::{Deserialize, Serialize};

/// The closed set of slow operations the queue carries.
///
/// Not an open string. A closed enum means a typo in a `submit` call site is a
/// compile error rather than a job that sits in the queue forever with a kind
/// nothing claims, and it means the ten kinds can be enumerated for the
/// dashboard's per-kind progress without a `SELECT DISTINCT`.
///
/// `Plugin` is the escape hatch for stash#5944 (plugins submit into the same
/// queue). It is deliberately *not* a free-form string: a plugin names itself,
/// and the queue treats every plugin job the same way, so a plugin cannot opt
/// out of the skip list or the retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Scan,
    Generate,
    Identify,
    Match,
    Cluster,
    Transcode,
    Autotag,
    ClusterRefresh,
    Backup,
    IndexSync,
    /// Work submitted by a plugin. The name lives in the job's `dedupe_key`.
    Plugin,
}

impl JobKind {
    /// All ten, in the ticket's order, plus `Plugin`.
    pub const ALL: [JobKind; 11] = [
        JobKind::Scan,
        JobKind::Generate,
        JobKind::Identify,
        JobKind::Match,
        JobKind::Cluster,
        JobKind::Transcode,
        JobKind::Autotag,
        JobKind::ClusterRefresh,
        JobKind::Backup,
        JobKind::IndexSync,
        JobKind::Plugin,
    ];

    /// The string in the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            JobKind::Scan => "scan",
            JobKind::Generate => "generate",
            JobKind::Identify => "identify",
            JobKind::Match => "match",
            JobKind::Cluster => "cluster",
            JobKind::Transcode => "transcode",
            JobKind::Autotag => "autotag",
            JobKind::ClusterRefresh => "cluster_refresh",
            JobKind::Backup => "backup",
            JobKind::IndexSync => "index_sync",
            JobKind::Plugin => "plugin",
        }
    }

    /// Parse a stored kind. An unknown kind is a bug, not a runtime condition
    /// to paper over: it means a newer build wrote a row this one cannot run,
    /// and silently treating it as `Scan` would be worse than refusing.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// How many workers may run this kind for *one target* at once.
    ///
    /// One, because "one job type per file" (stash#2824, #5709) is a
    /// statement about a file: a file must not be generated and transcoded
    /// simultaneously, and the cheapest way to guarantee that is one worker
    /// per (kind, target) pair.
    ///
    /// It is deliberately *not* one per kind across the whole library. That
    /// reading makes the 100k-item target unreachable -- a thousand files
    /// waiting to be generated, one at a time -- and is not what the issue
    /// describes. Different files are independent work; the conflict is
    /// between two jobs on the *same* file.
    pub const fn per_target_concurrency(self) -> usize {
        1
    }
}

impl fmt::Display for JobKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One unit of work, as submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// What kind of work this is.
    pub kind: JobKind,
    /// The idempotency key. Submitting the same key twice produces one job.
    ///
    /// This is the whole of stash#2913's first half and the plugin story's
    /// safety net. The convention the queue's helpers build is
    /// `"{kind}:{target}"`, so a watcher firing fifty times for one file
    /// produces one job — but the key is a plain string on purpose, because a
    /// plugin needs to scope it its own way.
    pub dedupe_key: String,
    /// What the job operates on: a file id, an object id, a directory.
    pub target_id: Option<String>,
    /// Free-form arguments, as JSON.
    pub payload: Option<serde_json::Value>,
}

impl JobSpec {
    /// A job with no payload.
    pub fn new(kind: JobKind, target_id: impl Into<String>) -> Self {
        let target_id = target_id.into();
        JobSpec {
            kind,
            dedupe_key: format!("{kind}:{target_id}"),
            target_id: Some(target_id),
            payload: None,
        }
    }

    /// A job with a payload.
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Override the dedupe key.
    ///
    /// The main reason to use this is a plugin that wants two jobs for one
    /// target, or a scan that wants one job per directory rather than per
    /// library. A job with a key that does not start with its kind's name
    /// sorts oddly in a log; that is cosmetic and not worth a validation
    /// error.
    pub fn dedupe_key(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = key.into();
        self
    }
}

/// A job as it exists in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub dedupe_key: String,
    pub target_id: Option<String>,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub payload: Option<serde_json::Value>,
}

impl Job {
    /// Is this job finished, one way or another?
    ///
    /// Terminal means the queue will never touch it again without an explicit
    /// resubmit. `Skipped` is terminal and that is the entire point of it:
    /// stash#2913 and #6837 are both a job that never becomes terminal, so the
    /// queue never drains and the user's CPU is spent on a file that will
    /// never work.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            JobState::Done | JobState::Failed | JobState::Skipped | JobState::Cancelled
        )
    }

    /// The word to use about this job in a status line.
    pub fn state_word(&self) -> &'static str {
        self.state.as_str()
    }
}

/// Why a job failed, and what the queue did about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutcome {
    pub state: JobState,
    pub error: Option<String>,
    /// The delay before the next attempt, if the job will be retried.
    pub retry_in: Option<std::time::Duration>,
}

impl JobOutcome {
    /// The job worked.
    pub fn done() -> Self {
        JobOutcome {
            state: JobState::Done,
            error: None,
            retry_in: None,
        }
    }

    /// The job failed but will be tried again after `retry_in`.
    pub fn retry(error: impl Into<String>, retry_in: std::time::Duration) -> Self {
        JobOutcome {
            state: JobState::Queued,
            error: Some(error.into()),
            retry_in: Some(retry_in),
        }
    }

    /// The job failed for good.
    pub fn failed(error: impl Into<String>) -> Self {
        JobOutcome {
            state: JobState::Failed,
            error: Some(error.into()),
            retry_in: None,
        }
    }

    /// The job is not going to work and is being set aside.
    ///
    /// Distinct from `failed` because `Skipped` means "do not retry this in
    /// this run" and `Failed` means "this ran and did not work". Both are
    /// terminal, but only one of them is a decision about the future.
    pub fn skipped(error: impl Into<String>) -> Self {
        JobOutcome {
            state: JobState::Skipped,
            error: Some(error.into()),
            retry_in: None,
        }
    }
}
