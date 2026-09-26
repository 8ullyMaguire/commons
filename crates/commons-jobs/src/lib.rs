//! commons-jobs — the durable queue every slow operation goes through.
//!
//! See `docs/spec/commons-spec.md` §6.3 and T-P2-004 in
//! `docs/plans/implementation-plan.md`.
//!
//! The design is in two pieces and the split is deliberate:
//!
//! - [`queue`] is the *what*: jobs, their states, dedupe, retry, the skip
//!   list. Pure data plus a mutex, no threads, no async, no I/O. Everything
//!   subtle about job scheduling is here and is testable without sleeping.
//! - [`worker`] is the *who*: a bounded pool that claims jobs and runs
//!   handlers, a supervisor that keeps the pool fed, and subprocess
//!   supervision with reaping.
//!
//! A file that fails is `Skipped` and never retried in the same run. That is
//! the whole of stash#2913 and #6837, and it is the reason the queue is a
//! data structure first and a thread pool second.

pub mod queue;
pub mod spec;
pub mod supervise;
pub mod worker;

pub use queue::{JobQueue, QueueConfig, QueueSnapshot, QueueStats, RetryPolicy};
pub use spec::{Job, JobKind, JobOutcome, JobSpec};
pub use supervise::{Supervisor, SupervisorConfig, SupervisorHandle};
pub use worker::{Handler, WorkerPool};
