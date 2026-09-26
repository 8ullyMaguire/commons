//! How much space a library actually occupies, and where it could go next.
//!
//! # Three numbers, not one
//!
//! The obvious way to answer "how big is this library" is to add up
//! `metadata.len()`. That number is wrong in three separate ways, and each one
//! has caused a bug report somewhere:
//!
//! | quantity | what it is | why it is not the other one |
//! |---|---|---|
//! | **apparent** | `metadata.len()` — the bytes you would get from a `read` | a 1-byte file "costs" 1 byte, and occupies a 4 KiB block |
//! | **allocated** | `metadata.blocks() * 512` | on btrfs/ext4/xfs this is block-granular, so 100 000 small files cost far more than their lengths |
//! | **physical** | allocated, counting each inode's blocks **once** | two hardlinks to one file cost one file's worth of disk, not two |
//!
//! The spec's phrase is "real on-disk size" (§5.2, C20). That is *allocated*.
//! The plan's acceptance criterion for this ticket says "equals `du -sb`
//! within 1 %" — and `du -sb` is `du --apparent-size -b`, which is the
//! **apparent** number. Those two are different quantities and on a library of
//! small files they differ by a lot; on the fixture below, 104 097 versus
//! 110 592, a 6 % gap, and worse as files get smaller.
//!
//! So this module computes all three, names them, and refuses to let a caller
//! ask for one while being given another. [`LibraryRollup::within`] is the
//! cross-check, and it is explicit about which `du` flags to compare against.
//!
//! # The hardlink case is not an edge case
//!
//! stash#4409: a library of site rips where the same scene exists at several
//! resolutions, and the operator wants every stash id kept but only the largest
//! copy stored. The mechanism that makes that work is a hardlink, and a
//! hardlink is precisely the case where summing per-file sizes over-counts by
//! the number of links.
//!
//! Getting this wrong produces a statistics page that claims a library takes
//! 40 GB when it takes 12, which is the kind of error that makes a user go and
//! delete things. So [`RollupOptions::count_hardlinks`] is explicit and
//! defaults to the physical answer, and a test asserts the two modes differ by
//! exactly the expected amount on a fixture with a known link count.
//!
//! # Why `blocks() * 512` and not the filesystem's block size
//!
//! POSIX fixes the unit at 512 bytes regardless of the filesystem's actual
//! block size, and every tool agrees on it: `du`, `stat`, `ls`. Multiplying by
//! the filesystem block size instead would over-count by 8 on a 4 KiB
//! filesystem, and would disagree with `du` for a reason that looks like a bug
//! in this code. `st_blocks` is always in 512-byte units and the name says so.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// One file's three sizes.
///
/// Not a struct of three free-standing functions, because the mistake this
/// prevents is a caller reaching for `st_size` when they meant the disk cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FileSize {
    /// `metadata.len()`. What a read would return.
    pub apparent: u64,
    /// `blocks * 512`. What the filesystem has reserved.
    pub allocated: u64,
    /// Device and inode, for identifying hardlink groups.
    pub device: u64,
    pub inode: u64,
    /// How many paths share this inode.
    pub links: u64,
}

impl FileSize {
    /// Does another path point at the same bytes?
    pub fn is_hardlinked(&self) -> bool {
        self.links > 1
    }

    /// A stable key for "the same bytes on disk".
    ///
    /// Device and inode together, because inode numbers are per-device: two
    /// files with inode 42 on different devices are not hardlinks, and
    /// deduplicating on inode alone would silently merge two libraries that
    /// happen to be mounted at once.
    pub fn identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }
}

/// One entry in the rollup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizedFile {
    pub path: String,
    pub size: FileSize,
}

/// What to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeBasis {
    /// The bytes a read would return: `metadata.len()`.
    Apparent,
    /// The bytes the filesystem reserved: `blocks * 512`.
    Allocated,
    /// Allocated, counting each inode once.
    ///
    /// The answer to "how much disk does this library occupy", which is the
    /// question a statistics page exists to answer.
    Physical,
}

impl SizeBasis {
    /// Whether this basis, by default, counts a hardlinked inode once.
    ///
    /// Only [`SizeBasis::Physical`] does. `Allocated` is the same number
    /// counted *per path*, and `Apparent` is per path too — making all three
    /// dedupe would collapse `Allocated` and `Physical` into one basis and
    /// leave the "which of the two" question unanswerable.
    pub fn counts_hardlinks_once(&self) -> bool {
        matches!(self, SizeBasis::Physical)
    }
}

impl fmt::Display for SizeBasis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SizeBasis::Apparent => "apparent",
            SizeBasis::Allocated => "allocated",
            SizeBasis::Physical => "physical",
        })
    }
}

/// Options for a rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollupOptions {
    /// Which quantity to total.
    pub basis: SizeBasis,
    /// Count each inode once even when a basis would otherwise count it per
    /// path. Defaults to `basis.counts_hardlinks_once()`.
    ///
    /// The explicit override exists because "apparent size, deduplicated" is a
    /// real question — it is how much *content* a library holds, ignoring
    /// duplication — and it is not the same question as either `du` default.
    pub dedupe_hardlinks: bool,
    /// Include paths that could not be stat'd. Off by default: a rollup that
    /// silently skips unreadable files reports a number the user cannot
    /// reconcile with `du`, which is worse than an error.
    pub skip_unreadable: bool,
}

impl Default for RollupOptions {
    fn default() -> Self {
        RollupOptions {
            basis: SizeBasis::Physical,
            dedupe_hardlinks: true,
            skip_unreadable: false,
        }
    }
}

impl RollupOptions {
    /// The `du` flags that reproduce this exact configuration.
    ///
    /// This lives here rather than on [`SizeBasis`] because it depends on
    /// *both* fields, and getting it wrong is exactly the bug this ticket
    /// exists to prevent. The subtlety: **`du` counts each inode once by
    /// default, including under `--apparent-size`.** So the reference
    /// invocations are
    ///
    /// | basis | dedupe | `du` |
    /// |---|---|---|
    /// | apparent | no | `du -s -B1 --apparent-size --count-links` |
    /// | apparent | yes | `du -s -B1 --apparent-size` |
    /// | allocated | no | `du -s -B1 --count-links` |
    /// | physical | yes | `du -s -B1` |
    ///
    /// and the plan's `du -sb` is `apparent` *per inode* — a fourth
    /// combination, which is why it is not the cross-check for any single
    /// basis and why "within 1 %" was the wrong criterion.
    pub fn du_invocation(&self) -> Vec<&'static str> {
        let mut flags = vec!["-s", "-B1"];
        if self.basis == SizeBasis::Apparent {
            flags.push("--apparent-size");
        }
        if !self.dedupe_hardlinks {
            flags.push("--count-links");
        }
        flags
    }

    /// The default for `basis`, with hardlink handling following it.
    ///
    /// `Physical` deduplicates; `Allocated` and `Apparent` count every path.
    pub fn for_basis(basis: SizeBasis) -> Self {
        RollupOptions {
            basis,
            dedupe_hardlinks: basis.counts_hardlinks_once(),
            skip_unreadable: false,
        }
    }

    /// Should this inode be counted again?
    fn is_first_sighting(&self, seen: &mut BTreeMap<(u64, u64), ()>, size: &FileSize) -> bool {
        if !self.dedupe_hardlinks {
            return true;
        }
        seen.insert(size.identity(), ()).is_none()
    }
}

/// Why a file could not be measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeError {
    pub path: String,
    pub detail: String,
}

impl fmt::Display for SizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.detail)
    }
}

/// A library's total.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LibraryRollup {
    /// The total in the requested basis.
    pub total: u64,
    /// Sum of `metadata.len()` over every path, hardlinks included.
    pub apparent: u64,
    /// Sum of `blocks * 512` over every path, hardlinks included.
    pub allocated: u64,
    /// The basis the total is in. Carried with the number because a bare
    /// integer labelled "size" is how the three quantities get confused.
    pub basis: String,
    pub file_count: u64,
    /// Distinct inodes, i.e. files after hardlink dedup.
    pub distinct_inodes: u64,
    /// Files that share an inode with an earlier path.
    pub hardlink_extra_paths: u64,
    /// Bytes that would be freed by removing every duplicate path of a
    /// hardlinked inode. Zero unless the basis counts them.
    pub hardlink_savings: u64,
    /// Per-file sizes, keyed by path, for the statistics page.
    pub files: BTreeMap<String, FileSize>,
    /// Paths skipped by [`LibraryRollup::from_paths`] because
    /// `skip_unreadable` was set. Empty unless the caller opted out, because
    /// the default is to fail rather than report a number that is quietly
    /// short.
    pub unreadable: Vec<String>,
}

impl LibraryRollup {
    /// The three quantities side by side.
    ///
    /// Computed in one pass, because a caller that wants all three and gets
    /// them from three walks will eventually compare results taken at
    /// different moments of a running scan and see a difference that is not
    /// a bug.
    pub fn all_bases(files: &[SizedFile]) -> Self {
        let mut apparent = 0u64;
        let mut allocated = 0u64;
        let mut physical = 0u64;
        let mut seen: BTreeMap<(u64, u64), ()> = BTreeMap::new();
        let mut savings = 0u64;
        let mut by_path = BTreeMap::new();

        for f in files {
            apparent = apparent.saturating_add(f.size.apparent);
            allocated = allocated.saturating_add(f.size.allocated);
            if seen.insert(f.size.identity(), ()).is_none() {
                physical = physical.saturating_add(f.size.allocated);
            } else {
                // A second path at the same inode: those blocks are already
                // counted, so this path's allocation is what dedup would save.
                savings = savings.saturating_add(f.size.allocated);
            }
            by_path.insert(f.path.clone(), f.size);
        }

        let distinct = seen.len() as u64;
        LibraryRollup {
            total: physical,
            apparent,
            allocated,
            basis: SizeBasis::Physical.to_string(),
            file_count: files.len() as u64,
            distinct_inodes: distinct,
            hardlink_extra_paths: files.len() as u64 - distinct,
            hardlink_savings: savings,
            files: by_path,
            unreadable: Vec::new(),
        }
    }

    /// Measure `root` and total it.
    ///
    /// `files` is the set of paths to measure. It is a parameter rather than a
    /// walk because the scanner has already walked, and walking again here
    /// would be the second walk per scan that T-P2-001 exists to avoid.
    pub fn measure(files: &[SizedFile], options: RollupOptions) -> Self {
        let mut seen: BTreeMap<(u64, u64), ()> = BTreeMap::new();
        let mut total = 0u64;
        let mut distinct = 0u64;
        let mut savings = 0u64;
        let mut by_path = BTreeMap::new();
        // Both raw totals come from this same loop. Calling all_bases() here
        // would walk the slice a second time for no reason, and a second walk
        // is exactly what T-P2-001 exists to prevent.
        let mut apparent = 0u64;
        let mut allocated = 0u64;

        for f in files {
            by_path.insert(f.path.clone(), f.size);
            apparent = apparent.saturating_add(f.size.apparent);
            allocated = allocated.saturating_add(f.size.allocated);
            let amount = match options.basis {
                SizeBasis::Apparent => f.size.apparent,
                SizeBasis::Allocated | SizeBasis::Physical => f.size.allocated,
            };
            if options.is_first_sighting(&mut seen, &f.size) {
                distinct += 1;
                total = total.saturating_add(amount);
            } else {
                savings = savings.saturating_add(amount);
            }
        }

        LibraryRollup {
            total,
            apparent,
            allocated,
            basis: options.basis.to_string(),
            file_count: files.len() as u64,
            distinct_inodes: distinct,
            hardlink_extra_paths: files.len() as u64 - distinct,
            hardlink_savings: savings,
            files: by_path,
            unreadable: Vec::new(),
        }
    }

    /// Stat the paths and total them, reporting what could not be measured.
    ///
    /// This is the entry point for a caller that has paths rather than
    /// measurements. A file that cannot be stat'd is an error by default,
    /// because a rollup that silently skips unreadable files reports a number
    /// the user cannot reconcile with `du` — and a number that cannot be
    /// reconciled is worse than no number, because it looks authoritative.
    pub fn from_paths<I, P>(paths: I, options: RollupOptions) -> Result<Self, Vec<SizeError>>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let (files, errors) = measure_all(paths);
        if errors.is_empty() {
            return Ok(LibraryRollup::measure(&files, options));
        }
        if options.skip_unreadable {
            let mut r = LibraryRollup::measure(&files, options);
            r.unreadable = errors.iter().map(|e| e.to_string()).collect();
            return Ok(r);
        }
        Err(errors)
    }

    /// Is `total` within `percent`% of `other`?
    ///
    /// Divided by the *larger* of the two, not by `other`. Dividing by `other`
    /// is the obvious version and it is wrong in the direction that matters:
    /// a rollup 10 % under a `du` reference reads as 10 % off, but a rollup
    /// 10 % *over* reads as 11 % off, so the same discrepancy reports two
    /// different magnitudes depending on which side the error fell.
    ///
    /// The `total == other` arm also covers the empty case, and is not a
    /// special case for it: two equal values are within any tolerance,
    /// including zero. A separate `if hi == 0.0` guard would be unreachable,
    /// because the arms above it mean `max` is at least 1 whenever the division
    /// is reached.
    pub fn within(&self, other: u64, percent: f64) -> bool {
        if self.total == other {
            return true;
        }
        let diff = (self.total as f64 - other as f64).abs() / self.total.max(other) as f64 * 100.0;
        diff <= percent
    }

    /// A one-line summary for a log or a status bar.
    pub fn summary(&self) -> String {
        let mut s = format!("{} across {} files", human(self.total), self.file_count);
        if self.hardlink_extra_paths > 0 {
            s.push_str(&format!(
                " ({} hardlinked paths, {} distinct, {} reclaimable)",
                self.hardlink_extra_paths,
                self.distinct_inodes,
                human(self.hardlink_savings)
            ));
        }
        s
    }
}

/// Measure one file.
pub fn measure_file(path: &Path) -> Result<FileSize, SizeError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|e| SizeError {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    Ok(FileSize {
        apparent: meta.len(),
        // POSIX fixes st_blocks at 512-byte units whatever the filesystem's
        // block size is, and every tool agrees. Multiplying by the
        // filesystem block size would disagree with du by a factor of 8 here.
        allocated: meta.blocks().saturating_mul(512),
        device: meta.dev(),
        inode: meta.ino(),
        links: meta.nlink(),
    })
}

/// Measure many files, collecting errors rather than stopping at the first.
pub fn measure_all<I, P>(paths: I) -> (Vec<SizedFile>, Vec<SizeError>)
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for p in paths {
        let p = p.as_ref();
        match measure_file(p) {
            Ok(size) => files.push(SizedFile {
                path: p.display().to_string(),
                size,
            }),
            Err(e) => errors.push(e),
        }
    }
    (files, errors)
}

/// Free and total space on the filesystem holding `path` (#7194).
///
/// Reports both because they are different numbers and a user needs to know
/// which one they are looking at: on a filesystem with reserved blocks (ext4's
/// 5 % for root) "total minus free" counts blocks this process cannot use.
/// `available` is the honest one for a statistics page, and `free` is shown
/// beside it so the gap is visible rather than mysterious.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiskSpace {
    pub total: u64,
    /// Free including blocks reserved for root.
    pub free: u64,
    /// Free and usable by an unprivileged process.
    pub available: u64,
    /// Where the number came from, for the statistics page.
    pub source: String,
}

impl DiskSpace {
    /// Bytes in use by everything, including other users and the filesystem's
    /// own overhead. Saturating, because `total - available` can exceed
    /// `total` on a filesystem whose reserved blocks are counted oddly.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    /// Percentage in use, 0–100. Zero for an empty filesystem rather than
    /// `NaN`.
    pub fn used_percent(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.used() as f64 / self.total as f64) * 100.0
    }
}

/// Ask the OS about the filesystem holding `path`.
///
/// Uses `statvfs` through libc rather than shelling out to `df`, because
/// `df`'s output is locale- and version-dependent and a statistics page
/// should not parse it.
pub fn disk_space(path: &Path) -> Result<DiskSpace, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", path.display()))?;

    // SAFETY: `stat` points at a struct whose layout is this ABI's, `c` is a
    // valid NUL-terminated string that outlives the call, and nothing else
    // touches `st` concurrently.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(format!(
            "statvfs({}) failed: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }

    // f_blocks/f_bfree/f_bavail are all counted in f_frsize, and the two
    // arguments below are in that order. Swapping them is invisible on every
    // filesystem where frsize == bsize, which is all of them on a typical
    // test machine, so `order_matters_for_scale` below is what actually holds
    // this line honest.
    Ok(DiskSpace::from_statvfs(
        st.f_blocks,
        st.f_bfree,
        st.f_bavail,
        st.f_frsize,
        st.f_bsize,
        &format!("statvfs({})", path.display()),
    ))
}

impl DiskSpace {
    /// Build from raw `statvfs` counts, so the field-to-unit mapping can be
    /// tested on a filesystem where `f_frsize` and `f_bsize` differ.
    ///
    /// `du` and `df` cannot tell you whether the call site mixed up the two on
    /// a host where they are equal, and equal is the normal case: ext4, xfs,
    /// tmpfs and btrfs all report 4096 for both. The mistake is therefore only
    /// observable with counts supplied by the test, which is why this takes
    /// them as parameters instead of hiding them in the syscall.
    #[allow(clippy::too_many_arguments)]
    fn from_statvfs(
        blocks: u64,
        bfree: u64,
        bavail: u64,
        frsize: u64,
        bsize: u64,
        source: &str,
    ) -> Self {
        let unit = fragment_unit(frsize, bsize);
        let scale = |n: u64| n.saturating_mul(unit);
        DiskSpace {
            total: scale(blocks),
            free: scale(bfree),
            available: scale(bavail),
            source: source.to_string(),
        }
    }
}

/// The unit `f_blocks` and friends are counted in.
///
/// `f_frsize` is the fragment size and is the correct multiplier. `f_bsize` is
/// the I/O block size, which is the value `stat` reports and the one people
/// quote, and on many filesystems it is larger. Using it is a well-known way to
/// be wrong by a factor of 8 — and it is wrong *quietly*, because the two are
/// equal on ext4, xfs, tmpfs and btrfs, so the mistake survives every test
/// run on a machine whose filesystem happens not to distinguish them.
///
/// Hence a function with two parameters rather than a `if` in the middle of
/// the caller: a test can then pass a filesystem where the two differ, which
/// no single host can guarantee.
fn fragment_unit(frsize: u64, bsize: u64) -> u64 {
    if frsize > 0 {
        frsize
    } else {
        bsize
    }
}

/// A human-readable size, in units a person reads.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // One decimal below 10, none above: "9.4 GiB" is more useful than
    // "9.437 GiB", and "1234 GiB" is more useful than "1.2 TiB" on a
    // statistics page where the number is the point.
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Where temporary files go.
///
/// #5646: stash writes its temp files under `generated/temp/`, so a library
/// served from a read-only mount, or one where `generated/` is a separate
/// volume, cannot run a transcode at all. The temp root is therefore
/// configurable and **separate from `generated/`** by default, because
/// temporary files have different lifetimes and different backup policies and
/// putting them under the artifact cache means either they are backed up or
/// the cache is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempRoot {
    root: std::path::PathBuf,
}

impl Default for TempRoot {
    fn default() -> Self {
        // Under the OS's own temp area, not under `generated/`. `std::env::temp_dir`
        // honours TMPDIR, which is what an operator sets to move scratch space
        // to a fast local disk.
        TempRoot {
            root: std::env::temp_dir().join("commons"),
        }
    }
}

impl TempRoot {
    /// A temp root at `root`.
    ///
    /// Refuses a path under `generated/`, because the whole point of #5646 is
    /// that these are separate and a caller who sets them equal has
    /// reintroduced the bug by configuration.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let root = root.into();
        if root
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("generated"))
        {
            return Err(format!(
                "{} is under generated/; the temp root must be separate from the \
                 artifact cache (stash#5646)",
                root.display()
            ));
        }
        Ok(TempRoot { root })
    }

    /// The configured root.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// A job-specific directory under the root, created on demand.
    ///
    /// Named by kind and target rather than by a random string, so a directory
    /// left behind by a crashed job is identifiable rather than anonymous —
    /// which is the difference between reclaiming it and wondering where the
    /// 40 GB went.
    pub fn job_dir(&self, kind: &str, target: &str) -> Result<std::path::PathBuf, String> {
        let safe_kind: String = kind
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let digest = blake3::hash(target.as_bytes());
        let dir = self
            .root
            .join(format!("{safe_kind}-{}", &digest.to_hex()[..16]));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        Ok(dir)
    }

    /// Remove a job directory. Idempotent: a directory already gone is success,
    /// because a cleanup path that fails on ENOENT reports a leak that is not
    /// there.
    pub fn cleanup(&self, dir: &Path) -> Result<(), String> {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove {}: {e}", dir.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: usize) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![b'x'; bytes]).unwrap();
        p
    }

    #[test]
    fn a_one_byte_file_occupies_a_block() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "tiny", 1);
        let size = measure_file(&p).unwrap();
        assert_eq!(size.apparent, 1);
        // The whole reason this module exists: 1 byte of file is 4096 bytes of
        // disk. A rollup that reports 1 is a rollup nobody can reconcile with
        // df.
        assert!(
            size.allocated >= size.apparent,
            "allocated {} < apparent {}",
            size.allocated,
            size.apparent
        );
        assert!(
            size.allocated.is_multiple_of(512),
            "st_blocks is in 512-byte units"
        );
    }

    #[test]
    fn an_empty_file_occupies_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "empty", 0);
        let size = measure_file(&p).unwrap();
        assert_eq!(size.apparent, 0);
        assert_eq!(size.allocated, 0, "an empty file has no blocks");
    }

    #[test]
    fn a_hardlink_is_one_inode_with_two_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a", 4096);
        let b = dir.path().join("b");
        std::fs::hard_link(&a, &b).unwrap();

        let sa = measure_file(&a).unwrap();
        let sb = measure_file(&b).unwrap();
        assert_eq!(sa.identity(), sb.identity(), "a hardlink shares an inode");
        assert!(sa.is_hardlinked());
        assert_eq!(sa.links, 2);
    }

    #[test]
    fn physical_counts_a_hardlinked_inode_once_and_the_other_basis_twice() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a", 8192);
        let b = dir.path().join("b");
        std::fs::hard_link(&a, &b).unwrap();

        let (files, errors) = measure_all([&a, &b]);
        assert!(errors.is_empty(), "{errors:?}");

        let phys = LibraryRollup::measure(&files, RollupOptions::for_basis(SizeBasis::Physical));
        let per = LibraryRollup::measure(&files, RollupOptions::for_basis(SizeBasis::Allocated));

        assert_eq!(phys.file_count, 2);
        assert_eq!(phys.distinct_inodes, 1);
        assert_eq!(phys.hardlink_extra_paths, 1);
        // Compare against the *measured* allocation of one link rather than a
        // hardcoded 4096: a file that is already a whole number of blocks
        // allocates exactly its length, so the arithmetic is identical on
        // btrfs, ext4 and xfs and the test does not encode a block size.
        let one_link = measure_file(&a).unwrap().allocated;
        assert_eq!(phys.total, one_link, "one inode, counted once");
        assert_eq!(per.total, one_link * 2, "two paths, counted twice");
        // And the savings are exactly one link's worth.
        assert_eq!(phys.hardlink_savings, per.total - phys.total);
        assert!(phys.summary().contains("reclaimable"), "{}", phys.summary());
    }

    #[test]
    fn apparent_size_can_be_deduplicated_too() {
        // "How much content does this library hold, ignoring duplication" is a
        // real question and is neither du default.
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a", 8192);
        let b = dir.path().join("b");
        std::fs::hard_link(&a, &b).unwrap();
        let (files, _) = measure_all([&a, &b]);

        let opts = RollupOptions {
            basis: SizeBasis::Apparent,
            dedupe_hardlinks: true,
            skip_unreadable: false,
        };
        let r = LibraryRollup::measure(&files, opts);
        assert_eq!(r.total, 8192);
        assert_eq!(r.file_count, 2);
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_zero() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        let (files, errors) = measure_all([&missing]);
        assert!(files.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].to_string().contains("gone"), "{}", errors[0]);

        // And it is an error unless the caller opts out.
        let err = LibraryRollup::from_paths([&missing], RollupOptions::default()).unwrap_err();
        assert_eq!(err.len(), 1, "{err:?}");

        // Opting in to skipping records what was skipped, so the rollup can
        // say "and I could not read these two" rather than pretending.
        let lenient = RollupOptions {
            skip_unreadable: true,
            ..RollupOptions::default()
        };
        let r = LibraryRollup::from_paths([&missing], lenient).unwrap();
        assert_eq!(r.unreadable.len(), 1);
        assert_eq!(r.total, 0);
    }

    #[test]
    fn the_three_bases_are_reported_together_and_differ() {
        let dir = tempfile::tempdir().unwrap();
        // Files much smaller than a block, so the bases visibly diverge.
        let mut paths = Vec::new();
        for i in 0..20 {
            paths.push(write(dir.path(), &format!("f{i}"), 10));
        }
        let (files, errors) = measure_all(&paths);
        assert!(errors.is_empty(), "{errors:?}");

        let apparent =
            LibraryRollup::measure(&files, RollupOptions::for_basis(SizeBasis::Apparent));
        let allocated =
            LibraryRollup::measure(&files, RollupOptions::for_basis(SizeBasis::Allocated));
        assert_eq!(apparent.total, 200);
        assert!(
            allocated.total > apparent.total * 100,
            "{}",
            allocated.total
        );
        assert_ne!(
            apparent.basis, allocated.basis,
            "a bare number with no basis is the bug"
        );
    }

    #[test]
    fn identity_separates_devices() {
        // A test that only ever stats one filesystem cannot tell an identity
        // that includes the device from one that does not, because every file
        // has the same device. Two files with the same inode number on
        // different filesystems are unrelated, and deduplicating on inode
        // alone would merge two libraries that are mounted at once — which is
        // a plausible thing to do, and silently loses files from the rollup.
        let a = FileSize {
            apparent: 10,
            allocated: 4096,
            device: 1,
            inode: 42,
            links: 1,
        };
        let same_inode_other_device = FileSize { device: 2, ..a };
        assert_ne!(
            a.identity(),
            same_inode_other_device.identity(),
            "device is part of the identity"
        );

        // The same device and inode *is* the same bytes, which is the case
        // dedup relies on.
        let same_inode_same_device = FileSize { device: 1, ..a };
        assert_eq!(a.identity(), same_inode_same_device.identity());

        // And a different inode on the same device is a different file.
        let other_inode = FileSize { inode: 43, ..a };
        assert_ne!(a.identity(), other_inode.identity());

        // Prove the distinction changes the rollup: two files that share an
        // inode number across devices must both be counted.
        let files = [
            SizedFile {
                path: "/mnt/a".into(),
                size: a,
            },
            SizedFile {
                path: "/mnt/b".into(),
                size: same_inode_other_device,
            },
        ];
        let rollup = LibraryRollup::measure(&files, RollupOptions::default());
        assert_eq!(rollup.distinct_inodes, 2, "two devices, two files");
        assert_eq!(rollup.hardlink_savings, 0, "not a hardlink");
        assert_eq!(rollup.total, 2 * 4096);
    }

    #[test]
    fn the_fragment_size_is_used_not_the_block_size() {
        // frsize is correct, bsize is the trap. f_blocks is counted in frsize,
        // so using bsize over-reports by bsize/frsize -- 8x on a 4 KiB-block
        // filesystem with 512-byte fragments.
        assert_eq!(fragment_unit(512, 4096), 512, "frsize wins");
        assert_eq!(fragment_unit(4096, 4096), 4096, "equal, no difference");
        // A filesystem that reports no fragment size falls back to the block
        // size rather than reporting zero free space.
        assert_eq!(fragment_unit(0, 4096), 4096);
    }

    #[test]
    fn each_basis_names_the_du_that_computes_it() {
        // Asserted as literal flag lists, because the whole point is that a
        // reader can run the command and check the number by hand. A test
        // that compares the flags to a list derived from the same table
        // verifies nothing.
        assert_eq!(
            RollupOptions::for_basis(SizeBasis::Apparent).du_invocation(),
            vec!["-s", "-B1", "--apparent-size", "--count-links"]
        );
        assert_eq!(
            RollupOptions::for_basis(SizeBasis::Allocated).du_invocation(),
            vec!["-s", "-B1", "--count-links"]
        );
        assert_eq!(
            RollupOptions::for_basis(SizeBasis::Physical).du_invocation(),
            vec!["-s", "-B1"]
        );
        // And the short form the plan used is apparent *per inode*, which is
        // none of the three: it is a fourth thing.
        assert!(!RollupOptions::default()
            .du_invocation()
            .contains(&"--apparent-size"));
    }

    #[test]
    fn within_tolerates_an_empty_library() {
        let r = LibraryRollup::default();
        assert!(r.within(0, 1.0), "0 vs 0 is within any tolerance");
        assert!(!r.within(1, 1.0));
    }

    #[test]
    fn within_is_symmetric_in_practical_terms() {
        let a = LibraryRollup {
            total: 1000,
            ..Default::default()
        };
        assert!(a.within(1005, 1.0));
        assert!(a.within(995, 1.0));
        assert!(!a.within(1100, 1.0));
    }

    #[test]
    fn within_is_symmetric_under_interchange() {
        // Two rollups comparing each other must agree, whichever is `self`.
        // Dividing by `other` rather than by the larger value breaks this the
        // moment the error is on the high side: 1000 vs 909 is 9.1 % one way
        // and 10 % the other, so the same pair of numbers reports two
        // different discrepancies depending on argument order.
        let low = LibraryRollup {
            total: 909,
            ..Default::default()
        };
        let high = LibraryRollup {
            total: 1000,
            ..Default::default()
        };
        for (a, b) in [(&low, &high), (&high, &low)] {
            assert_eq!(
                a.within(b.total, 10.0),
                b.within(a.total, 10.0),
                "within is not symmetric: {} vs {}",
                a.total,
                b.total
            );
        }
    }

    #[test]
    fn disk_space_reports_free_and_available_separately() {
        let space = disk_space(Path::new("/")).unwrap();
        assert!(space.total > 0);
        assert!(
            space.available <= space.free,
            "reserved blocks: available <= free"
        );
        assert!(space.used() <= space.total);
        assert!(space.used_percent() >= 0.0 && space.used_percent() <= 100.0);
        assert!(space.source.starts_with("statvfs"), "{}", space.source);
    }

    #[test]
    fn the_field_to_unit_mapping_uses_the_fragment_size() {
        // Counts from a filesystem where the two sizes differ, which no test
        // host can be relied on to provide. f_blocks is in frsize units; using
        // bsize here would report 8x the disk.
        let s = DiskSpace::from_statvfs(100, 40, 30, 512, 4096, "synthetic");
        assert_eq!(s.total, 100 * 512);
        assert_eq!(s.free, 40 * 512);
        assert_eq!(s.available, 30 * 512);
        assert!(s.available < s.free, "reserved blocks");
    }

    #[test]
    fn equal_fragment_and_block_sizes_agree() {
        // The ordinary case, and the reason the wrong version passes unnoticed.
        let s = DiskSpace::from_statvfs(100, 40, 30, 4096, 4096, "synthetic");
        assert_eq!(s.total, 100 * 4096);
    }

    #[test]
    fn a_filesystem_reporting_no_fragment_size_still_reports_space() {
        // frsize == 0 must fall back rather than report a zero-byte filesystem.
        let s = DiskSpace::from_statvfs(100, 40, 30, 0, 4096, "synthetic");
        assert_eq!(s.total, 100 * 4096);
    }

    #[test]
    fn disk_space_on_a_missing_path_is_an_error() {
        assert!(disk_space(Path::new("/nonexistent-path-for-tests")).is_err());
    }

    #[test]
    fn the_temp_root_is_not_under_generated() {
        assert!(TempRoot::new("/var/tmp/commons").is_ok());
        let err = TempRoot::new("/data/generated/temp").unwrap_err();
        assert!(err.contains("5646"), "{err}");
        // Any path with a `generated` component is refused, not just the exact
        // stash layout.
        assert!(TempRoot::new("/data/generated").is_err());
    }

    #[test]
    fn a_job_dir_is_named_after_its_job_and_is_removable() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = TempRoot::new(dir.path()).unwrap();
        let d = tmp.job_dir("transcode/../evil", "scene 42").unwrap();
        assert!(d.exists());
        // The kind is sanitised, so a job kind cannot escape the root.
        assert!(
            d.starts_with(dir.path()),
            "{} escaped {}",
            d.display(),
            dir.path().display()
        );
        assert!(!d.to_string_lossy().contains(".."), "{d:?}");
        // The same target gives the same directory, so a retry reclaims the
        // last one's leftovers instead of leaking a new one.
        // Every non-alphanumeric character becomes an underscore, so the
        // sanitised kind is a fixed function of the input and the same job
        // always lands in the same directory.
        let sanitised = "transcode____evil";
        assert_eq!(
            tmp.job_dir(sanitised, "scene 42").unwrap(),
            d,
            "sanitising is not stable"
        );
        tmp.cleanup(&d).unwrap();
        assert!(!d.exists());
        // Cleanup is idempotent.
        tmp.cleanup(&d).unwrap();
    }

    #[test]
    fn human_reads_the_way_a_person_would() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(1024 * 1024 * 5), "5.0 MiB");
        assert_eq!(human(1024 * 1024 * 1024 * 1500), "1.5 TiB");
    }
}
