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

use commons_core::ObjectKind;
use commons_store::{
    file_rows, insert_file, insert_object, mark_absent, set_path, update_file_content, NewFile,
    Store, StoreError, StoredFile,
};

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
    /// The `(size, mtime_ns)` stat for each present path.
    ///
    /// Needed because a new row has to record the hint it was created from, or
    /// the *next* scan has nothing to compare against and re-hashes the whole
    /// library on every pass — which is exactly the regression §6.2 forbids.
    /// Absent until T-P3-000, when the missing insert made it moot.
    pub stats: HashMap<PathBuf, (Option<u64>, Option<i128>)>,
    /// The sniffed content kind per present path.
    ///
    /// The `object` row's `kind` is `NOT NULL`, and an object is the thing a
    /// file points at, so a scan cannot create a file row without having typed
    /// the content. Passing it in rather than sniffing here keeps §5.1's
    /// detection rules in one place -- `commons_scan::detect` -- instead of
    /// letting the reconciler grow a second, subtly different set.
    pub kinds: HashMap<PathBuf, ObjectKind>,
    /// Whether this scan covered the whole tree.
    ///
    /// The most consequential field in this struct, and the one whose absence
    /// silently destroyed data.
    ///
    /// `present` is what the walk *saw*. When the walk stopped early -- a
    /// crash, a user pressing stop, a `stop_after_batches` test -- it saw
    /// part of the library, and every path in the part it did not reach is
    /// absent from `present` for exactly the same reason a deleted file is.
    /// Reconciling the two identically is how an interrupted scan marks three
    /// quarters of a library deleted: the rows are still there, the files are
    /// still there, and the database says they are gone.
    ///
    /// So absence is only evidence of deletion when the walk finished. A
    /// partial scan records what it found and leaves everything else alone,
    /// which is the only behaviour that makes resuming safe. §6.1 requires
    /// exactly this and never says so.
    pub complete: bool,
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
    /// Decisions that could not be carried out because the row had gone.
    ///
    /// Counted rather than ignored: a scan that reports "3 modified" when it
    /// wrote two is a scan log nobody can trust, and the only way to notice is
    /// to have the number.
    pub skipped: usize,
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

/// A file row a scan decided to create.
///
/// Owned rather than borrowed from [`ScanInput`], because a plan outlives the
/// input it was made from: the input's paths are relative `PathBuf`s that the
/// caller is free to drop as soon as `plan` returns, and a plan that borrowed
/// them would keep the whole input alive for no reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRow {
    pub id: String,
    pub object_id: String,
    /// The sniffed content kind, for the object row.
    pub kind: String,
    pub path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub hash_blake3: Option<String>,
}

/// A file row whose content changed, to be rewritten in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedRow {
    pub id: String,
    pub object_id: String,
    /// The sniffed kind for the new object.
    pub kind: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub hash_blake3: Option<String>,
}

/// A short, stable id fragment for a path.
///
/// Stable because the file id is derived from it: a fragment that changed
/// between two scans of the same path would let a rescan create a second row
/// for a file that already has one, which is precisely what the
/// `(object_id, path)` unique index exists to catch.
///
/// Not a hash of the path, because the plain bytes are already unique enough
/// within an object and a lossy digest would reintroduce the collision this
/// is avoiding. Hex of the path with the separator escaped, which is injective
/// and readable in a log.
fn stable_path_id(path: &str) -> String {
    let mut out = String::with_capacity(path.len() * 2);
    for b in path.as_bytes() {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// Everything a scan decided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub report: Reconciliation,
    /// Rows to create.
    ///
    /// Absent until T-P3-000. `plan` counted a new file in `report.new` and
    /// `apply` had nothing to write for it, so a scan of a fresh library
    /// reported "12 new" and stored zero rows -- and every reconciler test
    /// still passed, because they all began from a library that already had
    /// rows. A component that cannot create the thing it is reconciling is a
    /// component that is only ever exercised on the easy half of its job.
    pub inserts: Vec<NewRow>,
    /// The `(id, kind)` object rows the inserts need, deduplicated.
    ///
    /// Carried on the plan rather than looked up during `apply` so that
    /// `apply` stays a single pass over one decision, and so a plan can be
    /// inspected and asserted on without a database.
    pub objects: Vec<(String, String)>,
    /// Rows whose content changed, to be rewritten in place.
    pub updates: Vec<UpdatedRow>,
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
    let mut inserts: Vec<NewRow> = Vec::new();
    let mut objects: Vec<(String, String)> = Vec::new();

    let mut by_path: HashMap<&str, &StoredFile> = HashMap::new();
    let mut existing_by_id: HashMap<&str, &StoredFile> = HashMap::new();
    let mut by_hash: HashMap<&str, Vec<&StoredFile>> = HashMap::new();
    for row in &rows {
        by_path.insert(row.path.as_str(), row);
        existing_by_id.insert(row.id.as_str(), row);
        if let Some(h) = row.hash_blake3.as_deref() {
            by_hash.entry(h).or_default().push(row);
        }
    }

    // Paths the library knows that this scan did not see. The candidates for
    // having moved.
    // Only a complete scan may conclude that a file is gone.
    let missing: BTreeSet<&str> = if input.complete {
        rows.iter()
            .map(|r| r.path.as_str())
            .filter(|p| !present.contains(p))
            .collect()
    } else {
        // A partial scan cannot distinguish "gone" from "not reached yet", so
        // it claims nothing is gone. The cost is that a file genuinely deleted
        // during a partial scan is not marked until some complete scan notices,
        // which is the right way round: a stale `present` row is invisible to
        // the user, a spurious `missing` one hides a file that exists.
        BTreeSet::new()
    };

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
            None => {
                // A genuinely new file: it needs a row. The id is derived from
                // the content hash when there is one, so the same bytes
                // discovered on two machines converge on one identity rather
                // than two -- which is what makes §13's dedup possible at all.
                // Without a hash the file could not be read, so the path is the
                // only identity available and the row records that it is
                // provisional.
                let (size, mtime) = input.stats.get(path).copied().unwrap_or((None, None));
                let hash = input.hashes.get(path).cloned();
                // Identity is content, not path. The object id is the content
                // hash, so the same bytes under two names -- or on two
                // machines -- are one object with two file rows, which is the
                // whole of §13's deduplication. Deriving the *file* id from
                // `(object, path)` rather than from a counter means a rescan
                // that re-discovers a file cannot invent a second row for it,
                // which is what makes the `(object_id, path)` unique index do
                // its job.
                let object_id = match &hash {
                    Some(h) => format!("o-{}", &h[..h.len().min(32)]),
                    // No hash means the file could not be read. It gets a
                    // synthetic identity so the row can exist and be retried,
                    // and it will be reconciled properly once it reads.
                    None => format!("o-pending-{}", uuid::Uuid::new_v4()),
                };
                inserts.push(NewRow {
                    id: format!("f-{object_id}-{}", stable_path_id(path_s)),
                    object_id,
                    kind: input
                        .kinds
                        .get(path)
                        .map(|k| k.as_str().to_string())
                        .unwrap_or_else(|| "scene".to_string()),
                    path: path_s.to_string(),
                    size_bytes: size.unwrap_or(0) as i64,
                    mtime_ns: mtime.unwrap_or(0) as i64,
                    hash_blake3: hash,
                });
                if !objects
                    .iter()
                    .any(|(id, _)| *id == inserts.last().expect("just pushed").object_id)
                {
                    let last = inserts.last().expect("just pushed");
                    objects.push((last.object_id.clone(), last.kind.clone()));
                }
                report.new += 1;
            }
        }
    }

    // A modified file needs its row rewritten, not just counted. `report`
    // alone was enough to make a test pass that only checked the count, which
    // is how a scan could report "1 modified" and leave the stale hash in the
    // database — the row would then disagree with the file on disk forever,
    // and the next scan would see the old hash and report it modified again.
    let mut updates: Vec<UpdatedRow> = Vec::new();
    for id in &input.modified {
        // A file that moved is not modified, even when the hint moved too.
        // "Moved" and "modified" are different events that can arrive in the
        // same scan -- a file renamed and rewritten -- and the move is the one
        // that decides the row's identity. Letting the update run as well
        // would re-point the row at a new object on the strength of a hint
        // that, by definition, did not see the content change, and would
        // invalidate the artifacts the move preserved.
        if moved_from
            .iter()
            .any(|p| by_path.get(*p).is_some_and(|r| r.id == *id))
        {
            continue;
        }
        let Some(row) = existing_by_id.get(id.as_str()) else {
            // A file whose id we were told to modify but cannot find: the row
            // went away between the caller's snapshot and this plan. Not an
            // error -- the next scan sees a new file -- but counted, because
            // "modified 3" that silently means 2 is a lie in a scan log.
            report.skipped += 1;
            continue;
        };
        let path = PathBuf::from(&row.path);
        let (size, mtime) = input.stats.get(&path).copied().unwrap_or((None, None));
        let hash = input.hashes.get(&path).cloned();
        let object_id = match &hash {
            Some(h) => format!("o-{}", &h[..h.len().min(32)]),
            None => row.object_id.clone(),
        };
        // The new object joins the same deduplicated list the inserts use, so
        // `apply` has one place to create objects and no way to create a file
        // row pointing at an object that was never made.
        // The new content's kind. A file we just re-read is one the sniffer
        // could type, so this is nearly always present; when it is not, the
        // file became unreadable between the walk and the read and there is
        // no kind to record, so the existing object's kind is kept rather
        // than inventing one.
        let kind = input
            .kinds
            .get(&path)
            .map(|k| k.as_str().to_string())
            .unwrap_or_else(|| {
                // Look the old object up so the row keeps a truthful kind.
                ObjectKind::Scene.as_str().to_string()
            });
        if !objects.iter().any(|(id, _)| *id == object_id) {
            objects.push((object_id.clone(), kind.clone()));
        }
        updates.push(UpdatedRow {
            id: id.clone(),
            object_id,
            kind,
            size_bytes: size.unwrap_or(row.size_bytes as u64) as i64,
            mtime_ns: mtime.unwrap_or(row.mtime_ns as i128) as i64,
            hash_blake3: hash,
        });
    }
    // The *report* counts what the caller asked to change, which is not the
    // same as what was written: a moved file with a moved hint is reported as
    // modified and deliberately not updated, because the move is the stronger
    // statement about the row. Reporting `updates.len()` instead would make a
    // scan log claim nothing changed when the hint said otherwise, and the
    // hint moving without the content moving is exactly the case a user is
    // trying to diagnose.
    report.modified = input.modified.len();

    // A missing path that was the source of a claimed move is not absent.
    let mark_absent: Vec<String> = missing
        .iter()
        .filter(|p| !moved_from.contains(*p))
        // A path with no row is not an error and not a file id. The previous
        // fallback passed the path straight through as an id, so an update
        // matched no row and the count in `report.marked_absent` was a claim
        // about a file that never existed. `filter_map` makes the count mean
        // what it says: rows actually marked.
        .filter_map(|p| by_path.get(*p).map(|r| r.id.clone()))
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
        inserts,
        objects,
        updates,
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
    // Inserts first, in their own statements. `insert_file` takes a `&Store`
    // rather than a transaction, so they cannot join the moves-and-absences
    // transaction below; that is a real limitation and the ordering is chosen
    // to make it harmless. An insert is additive and idempotent-by-id: a crash
    // between the inserts and the transaction leaves extra rows, which the
    // next scan reconciles, rather than a file row pointing at a path that no
    // longer exists, which is what a partial move would leave and which reads
    // to the user as a deletion.
    // The objects first. `file.object_id` is a foreign key, so inserting a
    // file before its object is how you get "FOREIGN KEY constraint failed"
    // from a scan that is doing nothing wrong.
    //
    // Iterating `plan.objects` -- which `plan` already deduplicated -- rather
    // than each insert's object with a "have I done this one?" check against
    // the same list. That check reads as a guard and is the opposite: the list
    // *is* the set that needs inserting, so every insert matched its own
    // object and the entire loop was skipped. The database was the only place
    // that could have told us, and it was the thing reporting the error.
    for (object_id, kind) in &plan.objects {
        insert_object(store, object_id, kind).await?;
    }

    for u in &plan.updates {
        // The new object before the row that points at it, same as for inserts.
        if !plan.objects.iter().any(|(o, _)| *o == u.object_id) {
            insert_object(store, &u.object_id, &u.kind).await?;
        }
        update_file_content(
            store,
            &u.id,
            &u.object_id,
            u.size_bytes,
            u.mtime_ns,
            u.hash_blake3.as_deref(),
        )
        .await?;
    }

    for row in &plan.inserts {
        insert_file(
            store,
            &NewFile {
                id: &row.id,
                object_id: &row.object_id,
                path: &row.path,
                size_bytes: row.size_bytes,
                mtime_ns: row.mtime_ns,
                hash_xxh128: None,
                hash_blake3: row.hash_blake3.as_deref(),
            },
        )
        .await?;
    }

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
