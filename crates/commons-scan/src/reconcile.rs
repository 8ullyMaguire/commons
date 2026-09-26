//! Reconciling a walk against the `file` table: what is new, what changed,
//! what moved, and what is gone.
//!
//! # Why this is the expensive part
//!
//! A rename is the case the whole ticket turns on. Without move detection,
//! moving a file makes the scanner see one deletion and one addition: it
//! deletes the row, creates a new one with a new id, re-extracts every piece
//! of metadata, and regenerates every thumbnail, sprite sheet and proxy. On a
//! library where a user reorganises folders, that is the difference between a
//! few seconds and an evening of the disk grinding.
//!
//! The fix is content identity. A file whose path disappeared and whose
//! `hash_blake3` reappears elsewhere is the *same file*: the row keeps its
//! id, its `object_id`, and its artifacts, and only the path is rewritten.
//!
//! # The trap
//!
//! Two files with identical content are not one file. A user with two copies
//! of the same video has two objects, and merging them because their hashes
//! match would silently destroy one of them -- including its rating, its
//! tags, and its place in any list. So a move is only claimed when it is
//! unambiguous:
//!
//!   * the content hash matches exactly one indexed file, and
//!   * that file's path was *not* seen this scan, and
//!   * the new path is not already indexed.
//!
//! A hash matching two missing files is ambiguous and neither is claimed; the
//! count is reported so a user is not left wondering. The first version took
//! whichever row the query returned first, which made the outcome depend on
//! database row order -- a merge that happened on Tuesday and not on Wednesday.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use commons_store::{file_rows, mark_absent, set_path, Store, StoreError, StoredFile};

/// What the scan found.
#[derive(Debug, Default)]
pub struct ScanInput {
    /// Paths seen this scan, relative to the library root.
    pub present: BTreeSet<PathBuf>,
    /// The BLAKE3 content hash of each newly-seen path. A path whose hash
    /// could not be computed is absent here, and is treated as new -- which
    /// is correct, because we cannot claim a move without content identity.
    pub hashes: HashMap<PathBuf, String>,
    /// File ids whose content changed, as decided by the caller's
    /// `(mtime, size)` hint comparison. These keep their id and need their
    /// artifacts regenerated.
    pub modified: BTreeSet<String>,
}

/// What a rescan decided, for reporting and for a scan log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub new: usize,
    pub unchanged: usize,
    pub modified: usize,
    /// Paths rewritten in place. The row id and every artifact survived.
    pub moved: usize,
    /// Rows to mark absent. Never deleted: a file on an unmounted volume is
    /// not a file the user threw away, and deleting the row would also
    /// cascade away its artifacts.
    pub marked_absent: usize,
    /// Moves not claimed because the content hash was ambiguous.
    pub ambiguous: usize,
}

impl Reconciliation {
    /// One line for a scan log.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} new, {} unchanged, {} modified, {} moved, {} absent",
            self.new, self.unchanged, self.modified, self.moved, self.marked_absent
        );
        if self.ambiguous > 0 {
            s.push_str(&format!(" ({} ambiguous, not merged)", self.ambiguous));
        }
        s
    }
}

/// A move that was applied, for the caller's audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub file_id: String,
    pub object_id: String,
    pub from: String,
    pub to: String,
}

/// Everything a scan decided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub report: Reconciliation,
    /// Moves to apply: rewrite the path, keep everything else.
    pub moves: Vec<Move>,
    /// Rows whose content changed and whose artifacts must be regenerated.
    pub needs_artifacts: Vec<String>,
    /// Rows to mark `state = 'absent'`.
    pub mark_absent: Vec<String>,
}

/// Decide, without writing.
///
/// The decision has all the subtle cases and is worth being able to test
/// without a database round trip per row; [`apply`] is then a thin write.
pub async fn plan(store: &Store, input: &ScanInput) -> commons_store::Result<Reconciled> {
    let rows: Vec<StoredFile> = file_rows(store).await?;

    // Owned, because the lookup below borrows from it for the whole function
    // and a `&str` borrowed from `PathBuf::to_string_lossy` would not live
    // long enough. The first version used `Box::leak` to get around that,
    // which leaks one string per path per scan -- on a 100k library, forever.
    let present_owned: Vec<String> = input
        .present
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let present: BTreeSet<&str> = present_owned.iter().map(String::as_str).collect();

    let mut report = Reconciliation::default();
    let mut moves = Vec::new();

    let mut by_path: HashMap<&str, &StoredFile> = HashMap::new();
    let mut by_hash: HashMap<&str, Vec<&StoredFile>> = HashMap::new();
    for row in &rows {
        by_path.insert(row.path.as_str(), row);
        if let Some(h) = row.hash_blake3.as_deref() {
            by_hash.entry(h).or_default().push(row);
        }
    }

    // Paths the library knows that this scan did not see. The candidates for
    // having moved.
    let missing: BTreeSet<&str> = rows
        .iter()
        .map(|r| r.path.as_str())
        .filter(|p| !present.contains(p))
        .collect();

    let mut moved_from: BTreeSet<&str> = BTreeSet::new();

    for path in &input.present {
        let path_s: &str = &path.to_string_lossy();
        if by_path.contains_key(path_s) {
            report.unchanged += 1;
            continue;
        }

        // A new path. Is it the same bytes as exactly one thing that went
        // missing?
        let candidate = input
            .hashes
            .get(path)
            .and_then(|h| by_hash.get(h.as_str()))
            .and_then(|candidates| {
                let movable: Vec<&&StoredFile> = candidates
                    .iter()
                    .filter(|r| missing.contains(r.path.as_str()))
                    .collect();
                match movable.len() {
                    1 => Some(Some(*movable[0])),
                    0 => Some(None),
                    _ => {
                        report.ambiguous += 1;
                        None
                    }
                }
            })
            .flatten();

        match candidate {
            Some(prev) => {
                moved_from.insert(prev.path.as_str());
                moves.push(Move {
                    file_id: prev.id.clone(),
                    object_id: prev.object_id.clone(),
                    from: prev.path.clone(),
                    to: path_s.to_string(),
                });
                report.moved += 1;
            }
            None => report.new += 1,
        }
    }

    report.modified = input.modified.len();

    // A missing path that was the source of a claimed move is not absent.
    let mark_absent: Vec<String> = missing
        .iter()
        .filter(|p| !moved_from.contains(*p))
        .map(|p| {
            by_path
                .get(*p)
                .map(|r| r.id.clone())
                .unwrap_or_else(|| (*p).to_string())
        })
        .collect();
    report.marked_absent = mark_absent.len();

    let mut needs_artifacts: Vec<String> = input.modified.iter().cloned().collect();
    // A move must NOT need artifacts: the content is the same bytes, so the
    // thumbnails are still correct. Getting this wrong is invisible in a unit
    // test and is the entire cost this ticket exists to remove.
    for m in &moves {
        needs_artifacts.retain(|id| id != &m.file_id);
    }

    Ok(Reconciled {
        report,
        moves,
        needs_artifacts,
        mark_absent,
    })
}

/// Write a plan.
///
/// Every step in one transaction: a partially-applied reconcile leaves a file
/// row pointing at a path that no longer exists, which reads to the user as
/// the file having been deleted.
pub async fn apply(store: &Store, plan: &Reconciled) -> Result<(), StoreError> {
    // All three steps in one transaction. A partially-applied reconcile
    // leaves a file row pointing at a path that no longer exists, which reads
    // to the user as the file having been deleted -- the worst possible
    // outcome for a maintenance task that is supposed to be invisible.
    let mut tx = store.pool().begin().await.map_err(StoreError::Query)?;

    for m in &plan.moves {
        set_path(&mut tx, &m.file_id, &m.to).await?;
    }
    for id in &plan.mark_absent {
        mark_absent(&mut tx, id).await?;
    }

    tx.commit().await.map_err(StoreError::Query)?;
    Ok(())
}
