//! The scan pipeline: the one path by which a library gets populated
//! (T-P3-000, §6.1 and §6.3).
//!
//! # Why this module exists
//!
//! Phase 2 built every component of a scan pipeline — the walker (T-P2-001),
//! hashing and move detection (T-P2-002), volume state (T-P2-003), the job
//! queue (T-P2-004) — and each one is tested and correct. Nothing called them
//! in sequence, so there was no end-to-end behaviour to be wrong. §6.1's
//! promise that "a scan never re-hashes an unchanged file" is a property of a
//! *composition*, and the composition did not exist.
//!
//! # The one property that matters
//!
//! An unchanged library, scanned again, must produce **no writes and no
//! reads**. Not "the same rows afterwards" — a row rewritten with identical
//! values is still a write, and a lost update looks exactly like that. Not
//! "no new rows" — a row deleted and re-inserted has the same count. And not
//! merely "the same rows" — a re-hash laundered into a background job leaves
//! every row byte-identical. [`ScanOutcome::hashed`] is the number that settles
//! it, and it is a statement about the pipeline rather than about the
//! database.
//!
//! # How the pieces compose
//!
//! ```text
//!   walk ──▶ (path, mtime, size) per file
//!              │
//!              ├─▶ compare against the stored row ──▶ Rehash::decide
//!              │      Changed, or no row, or an unreadable stat
//!              │        └──▶ hash_file (one read) ──▶ store
//!              │      Unchanged ──▶ carry the stored hash forward, read nothing
//!              │
//!              └──▶ reconcile::plan ──▶ new / moved / modified / missing
//!                                          └──▶ apply
//! ```
//!
//! The order is the point. The hint is consulted *before* the hash, so the
//! expensive read is skipped rather than taken and thrown away — which is what
//! makes §6.1's promise true of the pipeline rather than of one component.
//!
//! # Why hashing happens here and not in a job
//!
//! The slow part is a job everywhere else in the system, and it is tempting to
//! do the same here. It does not work: the decision of *what* to hash has to be
//! made against the stored state, synchronously, and a job that re-reads the
//! hint would race with the walk — a file renamed between the walk and the job
//! would be seen as both a move and a deletion. The `mtime`/`size` hint is a
//! hint precisely because it is cheap and re-checkable; the decision to read is
//! where the consistency argument lives, so it stays in one place.
//!
//! Job submission for the artifacts *after* a scan — thumbnails, sprites,
//! phash — belongs to the caller, which is the layer that knows what the
//! library is for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use commons_store::{file_rows, Store, StoreError};

use crate::hashing::{hash_file, Rehash};
use crate::reconcile::{self, ScanInput};
use crate::walker::{self, Checkpoint, FoundFile, SkipRule, WalkConfig};

/// How a scan behaves.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Skip rules, passed to the walker.
    pub skip: Vec<SkipRule>,
    /// Files per walk batch.
    pub batch_size: usize,
    /// Resume from this checkpoint instead of starting at the root.
    pub resume_from: Option<Checkpoint>,
    /// Stop after this many *batches* of files.
    ///
    /// The shape of a user pressing stop and of a crash: the walk has produced
    /// part of the tree and a checkpoint, but no report. `Some(0)` is treated
    /// as `Some(1)`, because a zero that means "do nothing" is
    /// indistinguishable from a configuration mistake, and silently doing
    /// nothing is the worse failure.
    pub stop_after_batches: Option<usize>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        PipelineConfig {
            skip: Vec::new(),
            batch_size: 512,
            resume_from: None,
            stop_after_batches: None,
        }
    }
}

/// What one scan did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Files the walk yielded, whatever became of them.
    pub total: u64,
    /// Rows created.
    pub new: usize,
    /// Files recognised as unchanged and therefore **not read**.
    pub unchanged: usize,
    /// Rows whose content changed, keeping their id.
    pub modified: usize,
    /// Paths rewritten in place, keeping their id and hash.
    pub moved: usize,
    /// Rows marked missing. Never deleted.
    pub marked_absent: usize,
    /// Files actually read to compute a hash.
    ///
    /// Zero for a rescan of an unchanged library, and that is the §6.1
    /// promise stated as a number.
    pub hashed: usize,
    /// Volumes that could not be read — one entry per volume, not per file.
    pub volume_errors: Vec<(PathBuf, String)>,
    /// Where to resume from, if the scan did not finish.
    pub checkpoint: Option<Checkpoint>,
    /// Whether the scan stopped before covering the tree.
    pub interrupted: bool,
}

impl ScanOutcome {
    /// One line for a scan log.
    pub fn summary(&self) -> String {
        format!(
            "{} files: {} new, {} unchanged, {} modified, {} moved, {} missing, {} hashed",
            self.total,
            self.new,
            self.unchanged,
            self.modified,
            self.moved,
            self.marked_absent,
            self.hashed
        )
    }
}

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Scan `root` into `store`.
pub async fn scan(
    store: &Store,
    root: &Path,
    config: PipelineConfig,
) -> Result<ScanOutcome, PipelineError> {
    // Read the stored rows once. The whole point of the hint is that this map
    // answers "do I need to read this file?" without touching the filesystem,
    // so building it is the expensive part and it happens a single time rather
    // than once per file.
    let existing: BTreeMap<String, commons_store::StoredFile> = file_rows(store)
        .await?
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    // --- walk -------------------------------------------------------------
    // The walk is synchronous and the store is async, so the walk completes
    // (or reaches its batch limit) and the database work follows. That is a
    // real constraint and it shapes the interruption semantics: an interrupted
    // scan has walked part of the tree and written nothing, rather than having
    // written half of it. Half-written is much harder to resume from, and the
    // walker already hands back an exact checkpoint for the part that is done.
    let seen = Arc::new(Mutex::new(
        BTreeMap::<PathBuf, (Option<i128>, Option<u64>)>::new(),
    ));
    let batches = Arc::new(AtomicUsize::new(0));
    let checkpoint = Arc::new(Mutex::new(config.resume_from.clone()));

    // The walk is stopped by the walker's own cancel flag, not by the
    // callback declining to record. A callback that returns without recording
    // has already walked the files; it just throws the answer away, so the scan
    // reports fewer files than exist and the checkpoint still says they were
    // done -- a resume that skips them for good. The flag is checked at a batch
    // boundary by the loop that owns the pending stack, which is the only place
    // that can both stop and leave a resumable checkpoint behind.
    let cancel: walker::CancelFlag = Arc::new(AtomicBool::new(false));
    let limit = config.stop_after_batches.map(|n| n.max(1));

    let sink = {
        let (seen, batches, checkpoint, cancel, limit) = (
            seen.clone(),
            batches.clone(),
            checkpoint.clone(),
            cancel.clone(),
            limit,
        );
        walker::batch_sink(move |batch: &walker::Batch<'_>| {
            let n = batches.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            // This batch is recorded, and only then does the limit apply. A
            // limit of N means N batches, not N-1: counting the batch before
            // recording it is what makes "stop after 1 batch" return nothing
            // and read as an empty library.
            if let Some(limit) = limit {
                if n >= limit {
                    cancel.store(true, AtomicOrdering::SeqCst);
                }
            }
            let mut seen = seen.lock().expect("the seen map is never poisoned");
            for f in batch.files {
                seen.insert(f.relative.clone(), (f.mtime_ns, f.size));
            }
            *checkpoint.lock().expect("the checkpoint is never poisoned") =
                Some(batch.checkpoint.clone());
        })
    };

    let walk_cfg = WalkConfig {
        skip: config.skip,
        batch_size: config.batch_size,
        on_batch: Some(sink),
        cancel: Some(cancel),
        ..Default::default()
    };
    let mut w = walker::Walker::new(root).with_config(walk_cfg);
    if let Some(cp) = &config.resume_from {
        w = w.resume_from(cp.clone());
    }
    let report = w.walk();

    let seen = seen.lock().expect("the seen map is never poisoned").clone();
    let interrupted = report.stats.cancelled;
    // A walk that ran to completion is the only kind that may conclude a file
    // is gone. See `ScanInput::complete`.
    let complete = !interrupted && report.volume_errors.is_empty();
    let checkpoint = checkpoint
        .lock()
        .expect("the checkpoint is never poisoned")
        .clone();

    // A walk that found nothing because the root is not there is a volume
    // error, not a silent success: one entry for the volume, which is §6.1's
    // #5683 shape — a missing drive must not produce one error per file that
    // would have been in it, and must not mark the library absent.
    let mut volume_errors = report.volume_errors.clone();
    if seen.is_empty() && !root.is_dir() && volume_errors.is_empty() {
        volume_errors.push((
            root.to_path_buf(),
            "the library root is not a readable directory".to_string(),
        ));
    }

    // --- decide, per file, whether to read it ---------------------------
    let mut by_path: BTreeMap<&Path, &commons_store::StoredFile> = BTreeMap::new();
    for row in existing.values() {
        by_path.insert(Path::new(&row.path), row);
    }

    let mut input = ScanInput::default();
    let mut hashed = 0usize;
    let mut unchanged = 0usize;
    let mut to_hash: Vec<PathBuf> = Vec::new();

    for (rel, (mtime, size)) in &seen {
        input.present.insert(rel.clone());
        // Recorded so a new row stores the hint it was created from. Without
        // it the next scan has nothing to compare against and re-hashes the
        // entire library every pass.
        input.stats.insert(rel.clone(), (*size, *mtime));
        let stored = by_path.get(rel.as_path());

        // The hint comparison. A `None` on either side means the stat failed
        // or the row carries no stored hint, and the answer to "may I skip the
        // read?" is then no: a file that cannot be compared is a file that
        // must be hashed.
        let skip_read = match (stored, mtime, size) {
            (Some(row), Some(m), Some(s)) => {
                Rehash::decide(row.mtime_ns as i128, row.size_bytes as u64, *m, *s)
                    == Rehash::Unchanged
            }
            _ => false,
        };

        if skip_read {
            // Not read, not re-hashed. The stored hash is carried into the
            // reconciler so a move is still detectable by content: a file that
            // did not change but moved is still that file, and dropping its
            // hash here would turn every rename into a new file plus a
            // deletion.
            unchanged += 1;
            if let Some(row) = stored {
                if let Some(h) = &row.hash_blake3 {
                    input.hashes.insert(rel.clone(), h.clone());
                }
            }
        } else {
            // Re-read, so the content is not what the row says it is. A file
            // that already had a row and is being re-read is therefore
            // *modified* and needs its artifacts regenerated; one that had no
            // row is new, and `report.new` counts it. Only the first case goes
            // in `modified`, and putting every re-read file in both would make
            // a fresh scan of a fresh library regenerate artifacts for 100k
            // files that have none yet.
            if let Some(row) = stored {
                input.modified.insert(row.id.clone());
            }
            to_hash.push(rel.clone());
        }
    }

    // --- read only what the hint said to read ---------------------------
    for rel in &to_hash {
        hashed += 1;
        if let Ok(h) = hash_file(&root.join(rel)) {
            input.hashes.insert(rel.clone(), h.blake3);
        }
        // The content kind, sniffed from the head of the file. The `object`
        // row needs it -- `kind` is `NOT NULL` -- and a scan is the only place
        // that both has the file in hand and knows the whole tree, which is
        // what `.forcegallery` in an ancestor directory needs to be honoured.
        // A file that cannot be read has no kind; the row is created with the
        // scene default and reconciled once it reads.
        if let Ok(d) = crate::detect::detect(&root.join(rel)) {
            input.kinds.insert(rel.clone(), d.kind);
        }
        // A file that cannot be read has no hash here, so the reconciler
        // treats it as new. That is the correct direction to be wrong in: we
        // cannot claim to know what it is, and claiming otherwise would let a
        // permissions problem look like a move.
    }

    input.complete = complete;

    let plan = reconcile::plan(store, &input).await?;
    reconcile::apply(store, &plan).await?;

    Ok(ScanOutcome {
        total: seen.len() as u64,
        new: plan.report.new,
        unchanged,
        modified: plan.report.modified,
        moved: plan.report.moved,
        marked_absent: plan.report.marked_absent,
        hashed,
        volume_errors,
        checkpoint: if interrupted { checkpoint } else { None },
        interrupted,
    })
}

/// How many jobs have been submitted against this library.
///
/// Exposed so the idempotence test can assert a rescan submitted nothing, which
/// is the check a row comparison cannot make: a re-hash laundered into a job
/// leaves every row byte-identical and passes every other assertion.
pub async fn submitted_job_count(store: &Store) -> u64 {
    commons_store::job_rows(store)
        .await
        .map(|rows| rows.len() as u64)
        .unwrap_or(0)
}

/// The hint a file contributes to the decision: its mtime and size.
///
/// Named because "the mtime and size" is what every caller in this module means
/// by "the hint", and §6.2 is about whether consulting it is sound.
pub fn hint_of(f: &FoundFile) -> (Option<i128>, Option<u64>) {
    (f.mtime_ns, f.size)
}
