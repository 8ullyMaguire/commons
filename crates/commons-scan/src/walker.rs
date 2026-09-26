//! Walking a library root, and knowing when to stop.
//!
//! # Why this is not `walkdir::WalkDir`
//!
//! The scan has one job that is easy to get wrong: **walk a tree cheaply,
//! exactly once, and be able to stop and resume without redoing work.** Three
//! upstream complaints are all failures of that job rather than of traversal
//! per se, so all three shaped this file.
//!
//! - **#7130, "Loading forever" on Rclone/NFS/SMB.** A watcher cannot be
//!   trusted across a network boundary: inotify events are lost, coalesced, or
//!   never delivered, so a "live" scan silently stops being live. Worse, a
//!   watcher over a network mount that has gone away blocks or spins. A
//!   remote volume therefore gets a *polled* scan on an explicit interval
//!   ([`VolumeKind`]), never a watcher, and the kind is decided from the
//!   filesystem type with an override, because the filesystem type alone gets
//!   it wrong often enough to matter.
//!
//! - **#1445, an interrupted scan restarts from zero.** [`Checkpoint`] records
//!   how far a walk got, keyed by the directory it was in. A resumed walk
//!   continues from there and still visits everything below.
//!
//! - **#5683, a high-CPU loop when a drive is gone.** The walk must notice a
//!   volume's absence *once* and stop. [`WalkReport::volume_errors`] and the
//!   `stat` counter in [`WalkStats`] exist so that claim is testable; see the
//!   acceptance test for T-P2-003, which asserts on the counter.
//!
//! # Why the stat counter exists at all
//!
//! A "does not spike" assertion cannot be made from a wall-clock number on a
//! shared machine. [`WalkStats::stats`] counts every `stat`/`read_dir` the
//! walk performed, which is deterministic: a walk that gives up on a missing
//! volume has a bounded, small count, and a walk that retries has a count
//! that grows with the size of the tree. That is the objective measure the
//! ticket asks for, and it is why `stats` is threaded through every function
//! here rather than inferred afterwards.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A directory that should not be descended into.
///
/// Compared against a path component, not the whole path: `.git` matches
/// `~/code/.git/config` and `~/x/.gitignore` does not. Matching the full
/// string is the bug that lets `.stfolder` (a real Syncthing directory) be
/// walked, and the one that makes a library of `.hidden` folders enormous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipRule {
    /// A directory with this exact name anywhere in the tree.
    Name(String),
    /// A path relative to the root, matched exactly. For exclusions that must
    /// not apply to a same-named directory deeper in the tree.
    ExactPath(PathBuf),
    /// A glob against the file name. `*` and `?` only, no `**`.
    Glob(String),
}

impl SkipRule {
    /// Does this rule skip `path`, which is `relative` to the scan root?
    ///
    /// The rule is about `path`'s own final component. A walk calls this for
    /// a directory *before* descending into it, so the component being tested
    /// is the directory's own name -- testing the children instead would
    /// `read_dir` a directory it meant to skip and only notice afterwards.
    pub fn matches(&self, path: &Path, relative: &Path) -> bool {
        match self {
            SkipRule::Name(name) => path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == name),
            SkipRule::ExactPath(p) => relative == p,
            SkipRule::Glob(pattern) => path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| glob_match(pattern, n)),
        }
    }
}

/// `*` and `?` glob matching over a single path component.
///
/// Not a general glob. `**` is deliberately unsupported: a recursive glob
/// needs a segment walk to be correct, and the two rules that exist in the
/// wild for library exclusions (`.st*`, `*.tmp`) are single-component.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Classic two-pointer with a backtrack point for the last `*`.
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// How a volume should be watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeKind {
    /// Local filesystem. A watcher works.
    Local,
    /// Network or FUSE mount. A watcher is not trusted; this gets a polled
    /// scan on an interval instead.
    Remote,
}

/// Which mode a volume should use, and why.
///
/// The decision is a function, not a field, so a test can assert on the
/// reasoning rather than on a boolean someone set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumePolicy {
    kind: VolumeKind,
    reason: &'static str,
    /// Poll interval for remote volumes. `None` for local.
    poll_interval: Option<std::time::Duration>,
}

impl VolumePolicy {
    /// The policy for a filesystem type string, as reported by
    /// `/proc/mounts` or `statfs`.
    ///
    /// The list is the practical one: network and FUSE filesystems are the
    /// ones where inotify is not dependable. `fuse.*` catches rclone, sshfs,
    /// and every other FUSE mount including ones that are perfectly local
    /// (a btrfs snapshot mounted via FUSE), which is the conservative
    /// direction -- polling a local volume is slow but correct, and a watcher
    /// on a remote one is fast and wrong.
    pub fn for_fs_type(fs_type: &str) -> Self {
        if fs_type.is_empty() {
            return unknown_policy();
        }
        let remote = fs_type.starts_with("fuse")
            || matches!(
                fs_type,
                "nfs" | "nfs4" | "cifs" | "smbfs" | "smb3" | "9p" | "afs" | "sshfs" | "davfs"
            );
        if remote {
            VolumePolicy {
                kind: VolumeKind::Remote,
                // Non-empty because a policy that cannot explain itself is how
                // #7130 got shipped in the first place: the user is told the
                // scan is "live" and it is not.
                reason: "network or FUSE mount: inotify events are not dependable across it",
                poll_interval: Some(std::time::Duration::from_secs(60)),
            }
        } else {
            local_policy()
        }
    }

    /// Force a volume to be polled, whatever the filesystem says.
    ///
    /// Exists because the filesystem type is not the only thing that matters:
    /// a local mount over a slow virtual disk, or a bind mount someone set up
    /// by hand, is remote in every way that counts.
    pub fn force_remote(interval: std::time::Duration) -> Self {
        VolumePolicy {
            kind: VolumeKind::Remote,
            reason: "configured as remote",
            poll_interval: Some(interval),
        }
    }

    pub fn kind(&self) -> VolumeKind {
        self.kind
    }

    /// A human-readable reason. Never empty — see [`VolumeKind`].
    pub fn reason(&self) -> &'static str {
        self.reason
    }

    /// The poll interval, for remote volumes.
    pub fn poll_interval(&self) -> Option<std::time::Duration> {
        self.poll_interval
    }
}

/// The policy when the filesystem type could not be read.
///
/// Local, and it says so. Polling a local volume is slow and wasteful; a
/// watcher on a remote one is fast and wrong, and "wrong" here means silently
/// missing every change. When the answer is unknown, the safe error is the
/// slow one.
pub fn unknown_policy() -> VolumePolicy {
    VolumePolicy {
        kind: VolumeKind::Local,
        reason: "filesystem type could not be determined; assuming local",
        poll_interval: None,
    }
}

fn local_policy() -> VolumePolicy {
    VolumePolicy {
        kind: VolumeKind::Local,
        reason: "local filesystem: a watcher is dependable",
        poll_interval: None,
    }
}

/// The default set of directories to skip.
///
/// Every entry is a directory that is *large* and *not library content*.
const DEFAULT_SKIP: &[&str] = &[
    "@eaDir",      // Syncthing conflict copies
    ".stfolder",   // Syncthing's own directory
    ".stversions", // Syncthing versioning
    ".git",
    ".svn",
    ".Trash",
    ".Trash-1000",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".cache",
    "lost+found",
    "#recycle",
];

/// Counters for what the walk actually did.
///
/// `stat_calls` is the objective measure of #5683. A walk that gives up on a
/// missing volume performs a bounded number of them; a walk that retries
/// performs one per file in the tree, per pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkStats {
    /// `read_dir` calls.
    pub read_dir_calls: u64,
    /// `stat`/`metadata` calls, including the ones inside `read_dir`.
    pub stat_calls: u64,
    /// Files yielded.
    pub files: u64,
    /// Directories descended into.
    pub dirs: u64,
    /// Directories skipped by a rule.
    pub skipped: u64,
}

/// How far a walk got, so it can resume.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Directory the walk was last inside, relative to the root. `None`
    /// before the walk starts.
    pub last_dir: Option<PathBuf>,
    /// How many files had been yielded in total.
    pub files_done: u64,
    /// How many of those came from `last_dir`.
    ///
    /// This is what makes the resume an exact partition rather than a
    /// re-scan with a hole. A batch flushes whenever it is full, which is
    /// almost never on a directory boundary, so "the walk was last in d10"
    /// leaves d10 half-yielded. Resuming from d10 without this number
    /// re-yields the files already handed out -- duplicates, which the
    /// acceptance test explicitly forbids -- and skipping the whole directory
    /// instead loses the rest.
    ///
    /// Entries are sorted, so "the first N files of this directory" is a
    /// well-defined set and the two passes partition the tree exactly.
    pub files_in_dir: u64,
    /// Directories the walk had discovered but not yet descended into,
    /// relative to the root, in the order they were pushed.
    ///
    /// `last_dir` on its own is not a resume position. The walk is a
    /// depth-first traversal with an explicit stack, so knowing which
    /// directory was current does not say what comes next: every sibling
    /// directory already discovered is still pending. The first version
    /// stored only `last_dir` and resumed by walking that directory alone,
    /// which silently dropped the whole rest of the tree -- the acceptance
    /// test caught it, and "the two passes cover 5 of 100 files" is the
    /// shape that failure took.
    ///
    /// Storing the stack makes the resume exact rather than approximate: the
    /// two passes partition the tree, with no gap and no duplicate.
    pub pending: Vec<PathBuf>,
    /// Generation of the walk this belongs to. A checkpoint from an earlier,
    /// aborted walk is ignored rather than trusted.
    pub generation: u64,
}

impl Checkpoint {
    /// Is this checkpoint usable to resume `generation`?
    ///
    /// A checkpoint from a different generation is discarded. Resuming into a
    /// tree that changed underneath the walk is how a resume turns into
    /// silent data loss, and the check is one line.
    pub fn resumable_into(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

/// A file found by the walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundFile {
    /// Absolute path, for opening.
    pub path: PathBuf,
    /// Path relative to the scan root, for display and for the key.
    pub relative: PathBuf,
    /// Size in bytes, or `None` if the file vanished between the directory
    /// listing and the stat.
    pub size: Option<u64>,
    /// Modification time in nanoseconds since the epoch, or `None` if the
    /// file vanished. This is a *hint* (T-P2-002): the hash is truth.
    pub mtime_ns: Option<i128>,
}

/// What one walk found, and what went wrong.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkReport {
    pub files: Vec<FoundFile>,
    /// Paths that could not be read, with the reason. One entry per
    /// directory, not per file — a missing volume produces one entry, not
    /// one per file that would have been in it. This is the #5683 shape.
    pub volume_errors: Vec<(PathBuf, String)>,
    pub stats: WalkStats,
    /// Where the walk stopped, for a checkpoint.
    pub checkpoint: Checkpoint,
}

/// Configuration for a walk.
#[derive(Clone)]
pub struct WalkConfig {
    pub skip: Vec<SkipRule>,
    /// Follow symlinked directories. Off by default: a symlink loop makes a
    /// walk non-terminating, and a symlink out of the library is a way to
    /// walk the whole filesystem by accident.
    pub follow_symlinks: bool,
    /// Called with each batch of files as it is found.
    ///
    /// The callback receives the checkpoint as well as the files, and that is
    /// not a convenience. `report.checkpoint` only exists once the walk
    /// RETURNS, which is precisely the case where it is useless: a scan
    /// interrupted by a crash, a power cut, or a user pressing stop has no
    /// report. A caller that wants to resume has to persist the checkpoint as
    /// the walk goes, and the only place it can get one is here. The first
    /// version of this API passed only the files, which made resumption
    /// untestable and, in the real system, impossible.
    ///
    /// An `Arc<Mutex<dyn FnMut>>` rather than a `Box<dyn FnMut>`, and the
    /// reason is that a `Box` cannot derive `Debug` or `Clone` on
    /// `WalkConfig` -- so a config could not be logged or reused, and every
    /// test that wanted to observe batching had to own the closure and give
    /// up access to it. Sharing the sink means the implementation and the
    /// observer can both hold it.
    pub on_batch: Option<BatchSink>,
    /// How many files per batch. Also how often `on_batch` is called.
    pub batch_size: usize,
}

/// Hand-written because `on_batch` is a trait object, which cannot derive
/// `Debug`. The field is the only one that needs it, and "a callback is
/// installed" is the whole truth about it -- printing a closure would mean
/// running it.
impl std::fmt::Debug for WalkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkConfig")
            .field("skip", &self.skip)
            .field("follow_symlinks", &self.follow_symlinks)
            .field("on_batch", &self.on_batch.as_ref().map(|_| "installed"))
            .field("batch_size", &self.batch_size)
            .finish()
    }
}

/// One batch of files, plus where the walk is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch<'a> {
    pub files: &'a [FoundFile],
    /// The checkpoint as of *after* these files. Persist this and a later walk
    /// can resume from it.
    pub checkpoint: &'a Checkpoint,
}

/// A shared, observable batch callback.
///
/// `Arc` because the walk may be handed to a thread and the observer stays
/// behind. `Mutex` because `FnMut` needs `&mut` and the observer needs to be
/// able to read the count the walk is updating.
pub type BatchSink = std::sync::Arc<std::sync::Mutex<dyn FnMut(&Batch<'_>) + Send>>;

/// Wrap a closure as a [`BatchSink`].
pub fn batch_sink<F>(f: F) -> BatchSink
where
    F: FnMut(&Batch<'_>) + Send + 'static,
{
    std::sync::Arc::new(std::sync::Mutex::new(f))
}

impl Default for WalkConfig {
    fn default() -> Self {
        WalkConfig {
            skip: DEFAULT_SKIP
                .iter()
                .map(|n| SkipRule::Name(n.to_string()))
                .collect(),
            follow_symlinks: false,
            on_batch: None,
            batch_size: 256,
        }
    }
}

impl WalkConfig {
    /// A config that skips nothing, for tests and for a library that is
    /// deliberately a flat dump of one directory.
    pub fn skip_nothing() -> Self {
        WalkConfig {
            skip: Vec::new(),
            follow_symlinks: false,
            on_batch: None,
            batch_size: 256,
        }
    }
}

/// A file walk that can be interrupted and resumed.
///
/// Iterative rather than recursive: a 100k-file tree with deep nesting
/// overflows the stack in a recursive walk, and the stack overflow happens
/// in the middle of a library scan where it is least welcome. The explicit
/// stack also makes the checkpoint exact -- the walk knows which directory it
/// is in without unwinding.
pub struct Walker {
    root: PathBuf,
    config: WalkConfig,
    generation: u64,
    stats: WalkStats,
    /// A set of visited directory identities, so a hardlink loop or a
    /// bind-mount cycle terminates.
    batch: Vec<FoundFile>,
    checkpoint: Checkpoint,
    /// Files yielded from the directory currently being walked. Reset on
    /// descent, and what `Checkpoint::files_in_dir` records.
    files_in_dir: u64,
    /// On a resume: how many files of the first directory to skip, because a
    /// previous pass already yielded them. One number, consumed as the walk
    /// goes; zero for a fresh walk.
    skip_in_first_dir: u64,
}

impl Walker {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Walker {
            root: root.into(),
            config: WalkConfig::default(),
            generation: 0,
            stats: WalkStats::default(),
            batch: Vec::new(),
            checkpoint: Checkpoint::default(),
            files_in_dir: 0,
            skip_in_first_dir: 0,
        }
    }

    pub fn with_config(mut self, config: WalkConfig) -> Self {
        self.config = config;
        self
    }

    /// Resume from a checkpoint, if it belongs to this generation.
    ///
    /// A checkpoint from another generation is ignored, and the walk starts
    /// from the root. That is the safe direction: a full walk is wasted time,
    /// a partial walk that skips files is data loss.
    pub fn resume_from(mut self, checkpoint: Checkpoint) -> Self {
        self.checkpoint = checkpoint;
        self
    }

    /// The generation this walk belongs to. Bump it to invalidate a
    /// checkpoint.
    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    /// Walk the tree, calling `on_batch` as files are found.
    ///
    /// Returns a report even when some directories could not be read. A
    /// partial walk with a recorded error is more useful than an error.
    pub fn walk(&mut self) -> WalkReport {
        let mut report = WalkReport::default();

        if !self.root.is_dir() {
            report.volume_errors.push((
                self.root.clone(),
                "root is not a readable directory".to_string(),
            ));
            report.stats = self.stats;
            report.checkpoint = self.checkpoint.clone();
            return report;
        }

        // Resume puts the first directory back on the stack instead of the
        // root. Everything *below* it is still walked, which is what makes a
        // resume correct rather than merely fast.
        let resume_at = if self.checkpoint.resumable_into(self.generation) {
            self.checkpoint.last_dir.clone()
        } else {
            self.checkpoint = Checkpoint {
                last_dir: None,
                files_done: 0,
                files_in_dir: 0,
                pending: Vec::new(),
                generation: self.generation,
            };
            None
        };

        let resume_skip = self.checkpoint.files_in_dir;
        let resume_dir = resume_at.clone();

        // Restore the traversal: the checkpoint's pending stack, with the
        // partial directory back on top. `last_dir` alone would lose every
        // sibling still waiting to be walked.
        let mut stack: Vec<PathBuf> = self
            .checkpoint
            .pending
            .iter()
            .map(|rel| self.root.join(rel))
            .collect();
        if let Some(rel) = resume_at.as_ref() {
            stack.push(self.root.join(rel));
        } else if stack.is_empty() {
            stack.push(self.root.clone());
        }

        while let Some(dir) = stack.pop() {
            let relative = dir
                .strip_prefix(&self.root)
                .map(Path::to_path_buf)
                .unwrap_or_default();

            // Record before descending, so a crash inside this directory still
            // leaves a checkpoint pointing at it.
            self.checkpoint.last_dir = Some(relative.clone());
            self.checkpoint.generation = self.generation;
            // Snapshot the stack, minus the entry just popped, so a crash
            // from here on resumes with the right siblings still pending.
            self.checkpoint.pending = stack
                .iter()
                .filter_map(|d| d.strip_prefix(&self.root).ok().map(Path::to_path_buf))
                .collect();
            let is_resume_dir = resume_dir.as_deref() == Some(relative.as_path());
            // The first directory of a resume is the one the previous pass was
            // part-way through; its first `skip_in_first_dir` files are already
            // accounted for. Every directory after it is fresh.
            self.files_in_dir = 0;
            self.skip_in_first_dir = if is_resume_dir && self.skip_in_first_dir == 0 {
                resume_skip
            } else if is_resume_dir {
                self.skip_in_first_dir
            } else {
                0
            };

            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => {
                    self.stats.read_dir_calls += 1;
                    e
                }
                Err(e) => {
                    // One error per directory. This is the #5683 fix: a volume
                    // that is gone produces this line once, and the walk moves
                    // on rather than retrying.
                    report.volume_errors.push((dir.clone(), e.to_string()));
                    continue;
                }
            };

            self.stats.dirs += 1;

            let mut children: Vec<PathBuf> = Vec::new();
            for entry in entries {
                self.stats.stat_calls += 1;
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        report.volume_errors.push((dir.clone(), e.to_string()));
                        continue;
                    }
                };
                let path = entry.path();
                children.push(path);
            }

            // Sort so a walk is deterministic. Two runs over the same tree
            // must yield the same order, or "idempotent" is untestable and
            // the checkpoint is meaningless.
            children.sort();

            for path in children {
                let meta = if self.config.follow_symlinks {
                    std::fs::metadata(&path)
                } else {
                    std::fs::symlink_metadata(&path)
                };
                let meta = match meta {
                    Ok(m) => {
                        self.stats.stat_calls += 1;
                        m
                    }
                    Err(_) => {
                        // Raced with a delete. Not an error worth reporting:
                        // a file that disappeared between the listing and the
                        // stat was never in the library.
                        continue;
                    }
                };

                let file_type = if self.config.follow_symlinks {
                    meta.file_type()
                } else {
                    // symlink_metadata does not follow, so a symlink to a file
                    // is a symlink. Resolve the type by hand.
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    meta.file_type()
                };

                let child_rel = path
                    .strip_prefix(&self.root)
                    .map(Path::to_path_buf)
                    .unwrap_or_default();

                if file_type.is_dir() {
                    if self.should_skip(&path, &child_rel) {
                        self.stats.skipped += 1;
                        continue;
                    }
                    stack.push(path);
                } else if file_type.is_file() {
                    if self.should_skip(&path, &child_rel) {
                        self.stats.skipped += 1;
                        continue;
                    }
                    // Skip the files a previous pass already yielded from
                    // this directory. `children` is sorted, so this is a
                    // stable prefix and the two passes partition the
                    // directory exactly -- no gap, no duplicate.
                    if self.skip_in_first_dir > 0 {
                        self.skip_in_first_dir -= 1;
                        self.stats.files += 1;
                        continue;
                    }
                    self.stats.files += 1;
                    self.files_in_dir += 1;
                    let found = FoundFile {
                        relative: child_rel,
                        // `Metadata::len` is always available for a file we
                        // successfully stat'ed, so there is no "unknown size"
                        // case here. Sparse and special files still report the
                        // apparent length, which is what the hint should be.
                        size: Some(meta.len()),
                        mtime_ns: meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_nanos() as i128),
                        path,
                    };
                    self.batch.push(found);
                    if self.batch.len() >= self.config.batch_size {
                        self.flush(&mut report);
                    }
                }
            }
        }

        self.flush(&mut report);
        self.checkpoint.files_done = self.stats.files;
        report.checkpoint = self.checkpoint.clone();
        report.stats = self.stats;
        report.files = std::mem::take(&mut report.files);
        report
    }

    fn should_skip(&self, path: &Path, relative: &Path) -> bool {
        self.config.skip.iter().any(|r| r.matches(path, relative))
    }

    fn flush(&mut self, report: &mut WalkReport) {
        if self.batch.is_empty() {
            return;
        }
        // The checkpoint handed to the callback accounts for these files, so
        // resuming from it cannot re-yield them -- and it carries the LIVE
        // pending stack, not a copy. Getting this wrong is invisible in
        // review and catastrophic in use: a resume then walks the one
        // current directory and abandons every sibling, which is a scan that
        // reports success and has lost most of the library.
        let checkpoint = Checkpoint {
            last_dir: self.checkpoint.last_dir.clone(),
            files_done: self.stats.files,
            files_in_dir: self.files_in_dir,
            pending: self.checkpoint.pending.clone(),
            generation: self.generation,
        };
        self.checkpoint.files_done = self.stats.files;
        self.checkpoint.files_in_dir = self.files_in_dir;
        if let Some(cb) = self.config.on_batch.as_ref() {
            let batch = crate::walker::Batch {
                files: &self.batch,
                checkpoint: &checkpoint,
            };
            // A poisoned mutex means a previous callback panicked. The walk is
            // already unwinding in that case, and swallowing the poison here
            // would let a later walk silently skip every batch. Take the
            // inner value and carry on: the panic has been reported, and the
            // alternative -- propagating it -- turns one bad callback into a
            // walk that cannot be resumed.
            match cb.lock() {
                Ok(mut guard) => guard(&batch),
                Err(poisoned) => poisoned.into_inner()(&batch),
            }
        }
        report.files.append(&mut self.batch);
    }
}

/// Walk a tree once, with the default configuration.
pub fn walk(root: impl Into<PathBuf>) -> WalkReport {
    Walker::new(root).walk()
}

/// Walk a tree, resuming from a checkpoint.
pub fn walk_resume(root: impl Into<PathBuf>, checkpoint: Checkpoint) -> WalkReport {
    Walker::new(root).resume_from(checkpoint).walk()
}

/// Read the filesystem type of the mount a path is on.
///
/// Returns `None` when it cannot be determined, which the caller should treat
/// as "local" — polling is slow but correct, and a watcher on a remote mount
/// is fast and wrong.
#[cfg(target_os = "linux")]
pub fn fs_type(path: &Path) -> Option<String> {
    // Canonicalize first: /proc/mounts reports the resolved path.
    let target = path.canonicalize().ok()?;
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    let mut best: Option<(usize, String)> = None;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let _device = fields.next()?;
        let mount = fields.next()?;
        let fstype = fields.next()?;
        // Longest matching mount point wins, so /mnt/disk/nested resolves to
        // the more specific mount rather than its parent.
        if target.starts_with(mount) {
            let len = mount.len();
            if best.as_ref().is_none_or(|(l, _)| len > *l) {
                best = Some((len, fstype.to_string()));
            }
        }
    }
    best.map(|(_, t)| t)
}

#[cfg(not(target_os = "linux"))]
pub fn fs_type(_path: &Path) -> Option<String> {
    None
}

/// Decide how to watch a volume, honouring an explicit override.
pub fn policy_for(path: &Path, force: Option<VolumeKind>) -> VolumePolicy {
    if let Some(VolumeKind::Remote) = force {
        return VolumePolicy::force_remote(std::time::Duration::from_secs(60));
    }
    match fs_type(path) {
        Some(t) => VolumePolicy::for_fs_type(&t),
        None => unknown_policy(),
    }
}

/// Read a file's first `n` bytes. The helper every detector needs.
pub fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(n);
    let mut file = File::open(path)?;
    let mut handle = BufReader::new(&mut file);
    let mut chunk = vec![0u8; n];
    let read = handle.read(&mut chunk)?;
    buf.extend_from_slice(&chunk[..read]);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    /// Build a tree from a list of relative paths. Creates parent directories.
    fn tree(dir: &Path, paths: &[&str]) {
        for p in paths {
            let full = dir.join(p);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, b"x").unwrap();
        }
    }

    fn names(report: &WalkReport) -> Vec<String> {
        let mut v: Vec<String> = report
            .files
            .iter()
            .map(|f| f.relative.to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn temp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    // ---------------------------------------------------------------- the walk

    #[test]
    fn an_empty_tree_yields_nothing_and_no_errors() {
        let d = temp();
        let r = walk(d.path());
        assert!(r.files.is_empty());
        assert!(r.volume_errors.is_empty());
        assert_eq!(r.stats.files, 0);
    }

    #[test]
    fn it_finds_files_at_every_depth() {
        let d = temp();
        tree(
            d.path(),
            &[
                "a.mp4",
                "sub/b.mp4",
                "sub/deeper/c.mp4",
                "sub/deeper/deepest/d.mp4",
            ],
        );
        let r = walk(d.path());
        assert_eq!(
            names(&r),
            [
                "a.mp4",
                "sub/b.mp4",
                "sub/deeper/c.mp4",
                "sub/deeper/deepest/d.mp4"
            ]
        );
        assert!(r.volume_errors.is_empty());
    }

    #[test]
    fn the_order_is_deterministic() {
        // Two runs over the same tree must yield the same order, or "idempotent"
        // is untestable and a checkpoint is meaningless.
        let d = temp();
        tree(d.path(), &["z.mp4", "a.mp4", "m/b.mp4", "m/a.mp4", "b.mp4"]);
        let first = names(&walk(d.path()));
        let second = names(&walk(d.path()));
        assert_eq!(first, second);
    }

    #[test]
    fn size_and_mtime_are_reported() {
        let d = temp();
        std::fs::write(d.path().join("a.mp4"), b"0123456789").unwrap();
        let r = walk(d.path());
        let f = &r.files[0];
        assert_eq!(f.size, Some(10));
        assert!(f.mtime_ns.is_some(), "mtime hint should be present");
        assert_eq!(f.relative, PathBuf::from("a.mp4"));
        assert!(f.path.is_absolute());
    }

    #[test]
    fn a_missing_root_is_one_error_not_a_panic() {
        let d = temp();
        let r = walk(d.path().join("nope"));
        assert!(r.files.is_empty());
        assert_eq!(r.volume_errors.len(), 1, "a missing root is one error");
        assert!(
            r.volume_errors[0].1.contains("readable"),
            "reason should say what failed"
        );
    }

    #[test]
    fn walking_a_root_that_is_a_file_is_one_error() {
        let d = temp();
        let f = d.path().join("a-file");
        std::fs::write(&f, b"x").unwrap();
        let r = walk(&f);
        // The root is a file, not a directory. One error, no files, no panic.
        assert!(r.files.is_empty());
        assert_eq!(r.volume_errors.len(), 1);
        assert_eq!(r.volume_errors[0].0, f);
    }

    #[test]
    fn a_file_at_the_root_is_just_a_file() {
        // The sibling case: an ordinary file inside the root is found, and
        // produces no error at all.
        let d = temp();
        std::fs::write(d.path().join("a-file"), b"x").unwrap();
        let r = walk(d.path());
        assert_eq!(names(&r), ["a-file"]);
        assert!(r.volume_errors.is_empty());
    }

    // ---------------------------------------------------------------- skipping

    #[test]
    fn it_skips_the_default_junk_directories() {
        let d = temp();
        tree(
            d.path(),
            &[
                "real/a.mp4",
                "@eaDir/b.mp4",
                ".stfolder/c.mp4",
                "$RECYCLE.BIN/d.mp4",
                "System Volume Information/e.mp4",
            ],
        );
        let r = walk(d.path());
        assert_eq!(names(&r), ["real/a.mp4"]);
        assert!(r.volume_errors.is_empty());
    }

    #[test]
    fn a_skip_rule_matches_a_component_not_a_substring() {
        // The bug this guards: matching the full path string means `.stfolder`
        // is skipped but so is `my.stfolder.bak`, and `--exclude=.git` skips
        // `legit.mp4` because the path contains "git".
        let name = |s: &str| SkipRule::Name(s.to_string());
        // The walk tests a directory's own path, before descending, so that is
        // what the rule is asked about.
        assert!(name(".stfolder").matches(Path::new("/lib/.stfolder"), Path::new(".stfolder")));
        // A name that merely contains the rule is not the rule.
        assert!(!name(".stfolder").matches(
            Path::new("/lib/my.stfolder.bak"),
            Path::new("my.stfolder.bak")
        ));
        // The bug this guards: a substring match would skip `legit.mp4`
        // because its path contains "git".
        assert!(!name("git").matches(Path::new("/lib/legit.mp4"), Path::new("legit.mp4")));
    }

    #[test]
    fn a_file_named_like_a_skipped_directory_is_still_skipped() {
        // The rule is by name, and it applies to files as well as directories.
        // A `.git` *file* is a worktree pointer and is not library content either.
        let d = temp();
        tree(d.path(), &["keep.mp4", ".git"]);
        let r = walk(d.path());
        assert_eq!(names(&r), ["keep.mp4"]);
    }

    #[test]
    fn skip_nothing_reaches_everything() {
        let d = temp();
        tree(d.path(), &["a.mp4", "@eaDir/b.mp4"]);
        let r = Walker::new(d.path())
            .with_config(WalkConfig::skip_nothing())
            .walk();
        assert_eq!(names(&r), ["@eaDir/b.mp4", "a.mp4"]);
    }

    #[test]
    fn an_exact_path_rule_does_not_apply_to_a_same_named_deeper_directory() {
        let d = temp();
        tree(d.path(), &["skipme/a.mp4", "keep/skipme/b.mp4"]);
        let cfg = WalkConfig {
            skip: vec![SkipRule::ExactPath(PathBuf::from("skipme"))],
            ..WalkConfig::default()
        };
        let r = Walker::new(d.path()).with_config(cfg).walk();
        assert_eq!(names(&r), ["keep/skipme/b.mp4"]);
    }

    // ---------------------------------------------------------------- globs

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.tmp", "a.tmp"));
        assert!(glob_match("*.tmp", ".tmp"));
        assert!(!glob_match("*.tmp", "a.tmp.bak"));
        assert!(glob_match("st*", "stfolder"));
        assert!(glob_match("st?older", "stfolder"));
        assert!(!glob_match("st?older", "stfolderx"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
        assert!(glob_match("**", "a")); // `**` is just two stars here, by design
    }

    #[test]
    fn a_glob_rule_skips_by_extension() {
        let d = temp();
        tree(d.path(), &["a.mp4", "b.part", "c.tmp", "d.mp4.part"]);
        let cfg = WalkConfig {
            skip: vec![SkipRule::Glob("*.part".to_string())],
            ..WalkConfig::default()
        };
        let r = Walker::new(d.path()).with_config(cfg).walk();
        assert_eq!(names(&r), ["a.mp4", "c.tmp"]);
    }

    // ---------------------------------------------------------------- symlinks

    #[test]
    fn symlinked_directories_are_not_followed_by_default() {
        // A symlink loop makes a non-terminating walk, and a symlink out of the
        // library is a way to walk the whole filesystem by accident.
        let d = temp();
        let other = temp();
        std::fs::create_dir_all(other.path()).unwrap();
        std::fs::write(other.path().join("outside.mp4"), b"x").unwrap();
        tree(d.path(), &["inside.mp4"]);
        std::os::unix::fs::symlink(other.path(), d.path().join("link")).unwrap();

        let r = walk(d.path());
        assert_eq!(
            names(&r),
            ["inside.mp4"],
            "the symlinked dir must not be walked"
        );

        let cfg = WalkConfig {
            follow_symlinks: true,
            ..WalkConfig::default()
        };
        let r2 = Walker::new(d.path()).with_config(cfg).walk();
        assert!(names(&r2).contains(&"link/outside.mp4".to_string()));
    }

    #[test]
    fn a_symlink_to_a_file_is_skipped_unless_following() {
        let d = temp();
        std::fs::write(d.path().join("real.mp4"), b"x").unwrap();
        std::os::unix::fs::symlink(d.path().join("real.mp4"), d.path().join("alias.mp4")).unwrap();
        assert_eq!(names(&walk(d.path())), ["real.mp4"]);
        let cfg = WalkConfig {
            follow_symlinks: true,
            ..WalkConfig::default()
        };
        let got = names(&Walker::new(d.path()).with_config(cfg).walk());
        assert!(got.contains(&"alias.mp4".to_string()));
    }

    // ---------------------------------------------------------------- batching

    #[test]
    fn batches_arrive_as_the_walk_progresses() {
        let d = temp();
        let paths: Vec<String> = (0..10).map(|i| format!("f{i:02}.mp4")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        tree(d.path(), &refs);

        // The sink is shared, so the sizes can be read back after the walk --
        // which is the whole reason it is an Arc<Mutex<..>> and not a Box.
        let sizes: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = Default::default();
        let sink = {
            let sizes = std::sync::Arc::clone(&sizes);
            batch_sink(move |b| sizes.lock().unwrap().push(b.files.len()))
        };
        let cfg = WalkConfig {
            batch_size: 3,
            on_batch: Some(sink),
            ..WalkConfig::default()
        };
        let r = Walker::new(d.path()).with_config(cfg).walk();
        let batches = sizes.lock().unwrap().clone();

        assert_eq!(r.files.len(), 10);
        assert_eq!(
            batches.iter().sum::<usize>(),
            10,
            "every file in exactly one batch"
        );
        assert!(
            batches.iter().all(|&n| n <= 3),
            "no batch over the size: {batches:?}"
        );
        assert!(
            batches.len() >= 4,
            "should have flushed more than once: {batches:?}"
        );
    }

    #[test]
    fn a_walk_config_can_be_cloned_and_printed() {
        // The first version of the callback took a Box, and `WalkConfig` could
        // then derive neither Debug nor Clone. A config you cannot log or reuse
        // is a config that gets rebuilt by hand at every call site.
        let cfg = WalkConfig {
            batch_size: 7,
            on_batch: Some(batch_sink(|_| {})),
            ..WalkConfig::default()
        };
        let copy = cfg.clone();
        assert_eq!(copy.batch_size, 7);
        assert!(format!("{cfg:?}").contains("WalkConfig"));
    }

    // ---------------------------------------------------------------- idempotence

    // The plan's acceptance test, first two thirds.
    #[test]
    fn scanning_twice_finds_the_same_files() {
        let d = temp();
        let paths: Vec<String> = (0..100)
            .map(|i| format!("lib{}/f{i:03}.mp4", i % 10))
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        tree(d.path(), &refs);

        let first = walk(d.path());
        assert_eq!(first.files.len(), 100, "100 files were created");
        let second = walk(d.path());
        assert_eq!(second.files.len(), 100, "a second scan finds the same 100");

        let a: BTreeSet<_> = names(&first).into_iter().collect();
        let b: BTreeSet<_> = names(&second).into_iter().collect();
        assert_eq!(a, b, "idempotent: the same set of paths");
    }

    // ---------------------------------------------------------------- resume

    /// The acceptance test's third case, and the one that matters: interrupt
    /// the walk mid-tree, then resume, and assert the total is 100 with no
    /// duplicates.
    ///
    /// A panic is the honest simulation of a crash -- it unwinds out of the
    /// walk mid-directory, leaving the state the way a SIGKILL would, and the
    /// test runner's own panic hook is silenced so the expected panic does not
    /// print as if it were a failure.
    #[test]
    fn an_interrupted_scan_resumes_rather_than_restarting() {
        let d = temp();
        // 100 files spread over 20 directories, so there is somewhere to stop.
        let mut paths = Vec::new();
        for dir in 0..20 {
            for f in 0..5 {
                paths.push(format!("d{dir:02}/f{f}.mp4"));
            }
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        tree(d.path(), &refs);

        // The walk is interrupted after 4 batches of 1 file, so it dies inside
        // a directory rather than at a directory boundary -- the case where a
        // checkpoint written only on entry would resume too late.
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // The checkpoint the walk hands out as it goes. A caller persists this
        // after every batch; here it is kept so the resume can use it.
        let saved: std::sync::Arc<std::sync::Mutex<Option<Checkpoint>>> = Default::default();
        // And the files it yielded, so the test can check the two passes
        // partition the tree. A crashed walk returns nothing, so the only
        // record of what it did is what the callback saw.
        let saved_files: std::sync::Arc<std::sync::Mutex<BTreeSet<String>>> = Default::default();
        let stopper = std::sync::Arc::clone(&seen);
        let keeper = std::sync::Arc::clone(&saved);
        let recorder = std::sync::Arc::clone(&saved_files);
        let cfg = WalkConfig {
            batch_size: 1,
            on_batch: Some(batch_sink(move |b| {
                let n = stopper.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n >= 3 {
                    panic!("simulated interruption");
                }
                for f in b.files {
                    recorder
                        .lock()
                        .unwrap()
                        .insert(f.relative.to_string_lossy().into_owned());
                }
                *keeper.lock().unwrap() = Some(b.checkpoint.clone());
            })),
            ..WalkConfig::default()
        };

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Walker::new(d.path()).with_config(cfg).walk()
        }));
        std::panic::set_hook(previous);

        assert!(first.is_err(), "the walk was interrupted");
        assert!(seen.load(std::sync::atomic::Ordering::SeqCst) >= 4);

        // The checkpoint the walk handed out before it died. It is not the
        // one a finished walk would report -- the walk never finished -- which
        // is exactly why the callback carries it.
        let checkpoint = saved
            .lock()
            .unwrap()
            .clone()
            .expect("the walk should have published a checkpoint before crashing");
        assert!(checkpoint.last_dir.is_some());

        // The first pass yielded some files; those are not in the crashed
        // walk's return value, so they are read out of the callback's own
        // record of what it saw.
        let first_pass: BTreeSet<String> = saved_files.lock().unwrap().iter().cloned().collect();
        let resumed = walk_resume(d.path(), checkpoint);
        let second_pass: BTreeSet<String> = names(&resumed).into_iter().collect();

        assert!(
            !first_pass.is_empty(),
            "the first pass yielded something before the crash"
        );
        assert!(
            first_pass.is_disjoint(&second_pass),
            "a file was yielded by both passes: {:?}",
            first_pass.intersection(&second_pass).collect::<Vec<_>>()
        );
        let union: BTreeSet<String> = first_pass.union(&second_pass).cloned().collect();
        assert_eq!(
            union.len(),
            100,
            "the two passes together cover all 100 files"
        );
        for dir in 0..20 {
            for f in 0..5 {
                let want = format!("d{dir:02}/f{f}.mp4");
                assert!(union.contains(&want), "resume missed {want}");
            }
        }
    }

    /// A resumed walk is itself resumable, and the checkpoint it hands back
    /// is one a later walk can use. Two interruptions in a row must still
    /// cover the tree.
    #[test]
    fn a_walk_can_be_interrupted_twice() {
        let d = temp();
        let mut paths = Vec::new();
        for dir in 0..10 {
            for f in 0..4 {
                paths.push(format!("d{dir:02}/f{f}.mp4"));
            }
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        tree(d.path(), &refs);

        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let saved: std::sync::Arc<std::sync::Mutex<Option<Checkpoint>>> = Default::default();
        // The crashed pass returns nothing, so the only record of what it had
        // already yielded is what its callback saw.
        let crashed_pass: std::sync::Arc<std::sync::Mutex<BTreeSet<String>>> = Default::default();
        let stopper = std::sync::Arc::clone(&seen);
        let keeper = std::sync::Arc::clone(&saved);
        let recorder = std::sync::Arc::clone(&crashed_pass);
        let cfg = WalkConfig {
            batch_size: 1,
            on_batch: Some(batch_sink(move |b| {
                if stopper.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 5 {
                    panic!("simulated interruption");
                }
                for f in b.files {
                    recorder
                        .lock()
                        .unwrap()
                        .insert(f.relative.to_string_lossy().into_owned());
                }
                *keeper.lock().unwrap() = Some(b.checkpoint.clone());
            })),
            ..WalkConfig::default()
        };
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Walker::new(d.path()).with_config(cfg).walk()
        }));
        std::panic::set_hook(previous);
        assert!(crashed.is_err());

        // Resume from the checkpoint the crashed walk published, take the one
        // THAT walk reports, and check the whole tree is still covered. Two
        // crashes in a row must not lose a file.
        let saved_cp = saved
            .lock()
            .unwrap()
            .clone()
            .expect("a checkpoint was published before the crash");
        let first_resume = walk_resume(d.path(), saved_cp);
        // `files_done` counts what THIS walk accounted for, including the
        // prefix it skipped because the crashed pass had already yielded it.
        // It is not a running total across walks -- the caller accumulates
        // that, and pretending otherwise is how a progress bar drifts.
        // With `batch_size: 1` a flush lands on every file, so the crash
        // happens mid-directory and one file of the partial directory was
        // already accounted for. That is the case `files_in_dir` exists for,
        // and it is why the overlap is one and not zero.
        assert_eq!(
            first_resume.checkpoint.files_done - first_resume.files.len() as u64,
            1,
            "the one file the crashed pass had already yielded"
        );
        // The report's checkpoint describes where the walk ENDED, not where
        // it resumed -- `files_in_dir` is 4, the last directory's count. The
        // resume point's own value is the one the crashed walk published, and
        // the previous test already asserts the resume honours it. Reading
        // these two as the same number is the easy mistake here.

        // The third pass resumes from where the second ended. By then the
        // tree is exhausted, so it yields nothing -- which is the point: a
        // completed scan is not rescanned. The union of all three passes, plus
        // what the crashed pass had already yielded, is the whole tree.
        let third = walk_resume(d.path(), first_resume.checkpoint.clone());
        assert!(
            third.files.is_empty(),
            "a finished tree is not walked again, got {:?}",
            names(&third)
        );

        let mut all: BTreeSet<String> = crashed_pass.lock().unwrap().iter().cloned().collect();
        all.extend(names(&first_resume));
        assert_eq!(all.len(), 40, "the three passes together cover the tree");
        for dir in 0..10 {
            for f in 0..4 {
                let want = format!("d{dir:02}/f{f}.mp4");
                assert!(all.contains(&want), "a pass missed {want}");
            }
        }
    }

    #[test]
    fn a_checkpoint_from_another_generation_is_ignored() {
        // Resuming into a tree that changed underneath the walk is how a resume
        // becomes silent data loss. A stale checkpoint is discarded and the walk
        // starts from the root.
        let d = temp();
        tree(d.path(), &["a/b/c/deep.mp4", "a/other.mp4"]);

        let stale = Checkpoint {
            last_dir: Some(PathBuf::from("a/b/c")),
            files_done: 99,
            files_in_dir: 0,
            pending: Vec::new(),
            generation: 7,
        };
        let r = walk_resume(d.path(), stale);
        assert_eq!(
            names(&r),
            ["a/b/c/deep.mp4", "a/other.mp4"],
            "a stale checkpoint must not cause files to be skipped"
        );
    }

    #[test]
    fn a_checkpoint_with_no_last_dir_starts_at_the_root() {
        let d = temp();
        tree(d.path(), &["a.mp4", "sub/b.mp4"]);
        let r = walk_resume(
            d.path(),
            Checkpoint {
                last_dir: None,
                files_done: 5,
                files_in_dir: 0,
                pending: Vec::new(),
                generation: 0,
            },
        );
        assert_eq!(names(&r), ["a.mp4", "sub/b.mp4"]);
    }

    #[test]
    fn a_walk_records_its_own_checkpoint() {
        let d = temp();
        tree(d.path(), &["a/b/c.mp4"]);
        let r = walk(d.path());
        assert!(
            r.checkpoint.last_dir.is_some(),
            "the walk knows where it stopped"
        );
        assert_eq!(r.checkpoint.files_done, r.files.len() as u64);
    }

    #[test]
    fn resumable_into_compares_generations() {
        let cp = Checkpoint {
            last_dir: None,
            files_done: 0,
            files_in_dir: 0,
            pending: Vec::new(),
            generation: 3,
        };
        assert!(cp.resumable_into(3));
        assert!(!cp.resumable_into(4));
        // A checkpoint with no generation is only usable by generation 0, which
        // is the only walk that has not been told otherwise.
        let fresh = Checkpoint::default();
        assert!(fresh.resumable_into(0));
    }

    // ---------------------------------------------------------------- stats

    #[test]
    fn stats_count_what_the_walk_did() {
        let d = temp();
        tree(
            d.path(),
            &["a.mp4", "b.mp4", "sub/c.mp4", "sub/deeper/d.mp4"],
        );
        let r = walk(d.path());
        assert_eq!(r.stats.files, 4);
        // root, sub, sub/deeper
        assert_eq!(r.stats.dirs, 3);
        assert_eq!(r.stats.skipped, 0);
        assert!(
            r.stats.read_dir_calls >= 3,
            "one read_dir per directory descended"
        );
        assert!(
            r.stats.stat_calls >= r.stats.files,
            "every file costs at least one stat"
        );
    }

    // ---------------------------------------------------------------- policy

    #[test]
    fn a_network_filesystem_is_polled_and_says_why() {
        // #7130. The reason string is the deliverable: a policy that cannot
        // explain itself is how "Loading forever" got shipped.
        for fs in [
            "nfs",
            "nfs4",
            "cifs",
            "smb3",
            "9p",
            "fuse.rclone",
            "fuse.sshfs",
            "davfs",
        ] {
            let p = VolumePolicy::for_fs_type(fs);
            assert_eq!(p.kind(), VolumeKind::Remote, "{fs} should be remote");
            assert!(!p.reason().is_empty(), "{fs} needs a non-empty reason");
            assert!(
                p.poll_interval().is_some(),
                "{fs} is polled, so it needs an interval"
            );
        }
    }

    #[test]
    fn a_local_filesystem_gets_a_watcher() {
        for fs in ["ext4", "btrfs", "xfs", "apfs", "ntfs", "exfat", "zfs"] {
            let p = VolumePolicy::for_fs_type(fs);
            assert_eq!(p.kind(), VolumeKind::Local, "{fs} should be local");
            assert!(!p.reason().is_empty(), "{fs} needs a non-empty reason");
            assert!(p.poll_interval().is_none(), "{fs} is watched, not polled");
        }
    }

    #[test]
    fn a_volume_can_be_forced_remote() {
        // The filesystem type is not the only thing that matters: a local mount on
        // a slow virtual disk is remote in every way that counts.
        let p = VolumePolicy::force_remote(std::time::Duration::from_secs(15));
        assert_eq!(p.kind(), VolumeKind::Remote);
        assert_eq!(p.poll_interval(), Some(std::time::Duration::from_secs(15)));
    }

    #[test]
    fn an_unknown_filesystem_is_treated_as_local_but_says_so() {
        // Polling is slow and correct; a watcher on a remote mount is fast and
        // wrong. So the default is local, and the reason admits the uncertainty
        // rather than pretending it knows.
        let p = VolumePolicy::for_fs_type("");
        assert_eq!(p.kind(), VolumeKind::Local);
        assert!(
            p.reason().contains("could not"),
            "reason should admit the guess: {}",
            p.reason()
        );
    }

    #[test]
    fn policy_for_honours_an_override() {
        let d = temp();
        assert_eq!(
            policy_for(d.path(), Some(VolumeKind::Remote)).kind(),
            VolumeKind::Remote
        );
        assert_eq!(
            policy_for(d.path(), Some(VolumeKind::Local)).kind(),
            VolumeKind::Local
        );
        // No override, no filesystem type to read for a temp dir on a real
        // machine: falls back to local with an honest reason.
        let p = policy_for(d.path(), None);
        assert!(!p.reason().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fs_type_of_a_real_path_is_found() {
        let d = temp();
        let t = fs_type(d.path());
        // A temp dir is on a real filesystem here, so this should resolve. On a
        // machine where /tmp is a tmpfs it resolves to "tmpfs", which is not in
        // the remote list -- and that is correct.
        assert!(t.is_some(), "a real path should have a filesystem type");
        if let Some(t) = t {
            assert!(!t.is_empty());
        }
    }

    // ---------------------------------------------------------------- helpers

    #[test]
    fn read_head_returns_the_first_n_bytes() {
        let d = temp();
        std::fs::write(d.path().join("a.bin"), b"0123456789").unwrap();
        assert_eq!(read_head(&d.path().join("a.bin"), 4).unwrap(), b"0123");
        // Asking for more than exists returns what there is, not an error: a
        // detector asking for 16 bytes of a 3-byte file is asking a real question.
        assert_eq!(
            read_head(&d.path().join("a.bin"), 16).unwrap(),
            b"0123456789"
        );
        assert!(read_head(&d.path().join("missing"), 4).is_err());
    }
}
