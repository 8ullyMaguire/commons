//! T-P2-006 acceptance: the rollup agrees with `du`, and the temp root is
//! genuinely separate from `generated/`.
//!
//! The plan's criterion for this ticket says the rollup must equal `du -sb`
//! within 1 %. That was checked and it is **the wrong cross-check**: `du -sb` is
//! `du --apparent-size -b`, which is the *apparent* total, while the spec asks
//! for "real on-disk size" (§5.2, C20). On the fixture below the two differ by
//! more than 1 %, and the gap grows as files get smaller — a library of 4 KiB
//! JPEGs is nearly all block overhead.
//!
//! So the acceptance test cross-checks **each basis against the `du` flags that
//! compute that same basis**, which is what makes the numbers comparable at
//! all:
//!
//! | basis | `du` |
//! |---|---|
//! | apparent | `du -sb` |
//! | allocated (per path) | `du -s -B1 --count-links` |
//! | physical (per inode) | `du -s -B1` (the default: links counted once) |
//!
//! The third row is the one that matters, and it is why the hardlink fixture
//! exists: `du` counts a hardlinked inode once by default, so a rollup that
//! summed per-file sizes would disagree with `du` by exactly the duplicated
//! link — the #4409 error, in the direction that tells a user their 40 GB
//! library is 12 GB.

use commons_scan::size::{
    disk_space, human, measure_all, LibraryRollup, RollupOptions, SizeBasis, SizeError, TempRoot,
};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `du` and return its byte total for `dir`.
fn du(dir: &Path, flags: &[&str]) -> u64 {
    let out = Command::new("du")
        .args(flags)
        .arg(dir)
        .output()
        .expect("du is on PATH; if it is not, this test cannot verify anything");
    assert!(
        out.status.success(),
        "du {flags:?} {dir:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("du prints a total")
        .parse()
        .expect("du prints a number")
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).unwrap();
}

fn paths_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).expect("fixture dir") {
        let p = e.expect("dir entry").path();
        if p.is_file() {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// A fixture whose files are far smaller than a block, so the bases visibly
/// differ, plus a hardlink pair to exercise the physical basis.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // 30 files of 100 bytes: apparent 3 000, allocated 30 * one block.
        for i in 0..30 {
            std::fs::write(dir.path().join(format!("small{i:02}")), vec![b'x'; 100]).unwrap();
        }
        // One whole-block file, and a second path at the same inode.
        let whole = dir.path().join("whole");
        std::fs::write(&whole, vec![b'x'; 4096]).unwrap();
        std::fs::hard_link(&whole, dir.path().join("whole_link")).unwrap();
        Fixture { dir }
    }

    fn paths(&self) -> Vec<PathBuf> {
        paths_in(self.dir.path())
    }
}

#[test]
fn the_rollup_matches_du_for_each_basis_it_names() {
    let f = Fixture::new();
    let files = {
        let (files, errors) = measure_all(f.paths());
        assert!(errors.is_empty(), "{errors:?}");
        files
    };
    let dir = f.dir.path();

    // Each basis, cross-checked against the du invocation that computes the
    // same quantity. Exact equality, not 1 %: the two are summing the same
    // stat fields, so a difference means one of them is wrong, and a
    // tolerance would hide exactly that.
    for basis in [
        SizeBasis::Apparent,
        SizeBasis::Allocated,
        SizeBasis::Physical,
    ] {
        let opts = RollupOptions::for_basis(basis);
        let rollup = LibraryRollup::measure(&files, opts);
        let expected = du(dir, &opts.du_invocation());
        assert_eq!(
            rollup.total,
            expected,
            "{basis} rollup ({}) must equal `du {}` ({expected})",
            rollup.total,
            opts.du_invocation().join(" ")
        );
    }
}

#[test]
fn du_counts_an_inode_once_even_under_apparent_size() {
    // The fact that makes `du -sb` the wrong cross-check: it is apparent size
    // *per inode*, which is a fourth combination that is neither of the three
    // bases, so it cannot be the reference for any one of them.
    let f = Fixture::new();
    let dir = f.dir.path();

    let per_inode = du(dir, &["-s", "-B1", "--apparent-size"]);
    let per_path = du(dir, &["-s", "-B1", "--apparent-size", "--count-links"]);
    assert!(
        per_inode < per_path,
        "du should count the hardlinked inode once ({per_inode}) and twice ({per_path})"
    );

    // The short flag agrees with the long one; it is the same computation.
    assert_eq!(du(dir, &["-sb"]), per_inode);

    // And so the plan's criterion, checked against the basis the spec asks
    // for, fails. Asserting the failure is the point: it stops the wrong
    // criterion from being reintroduced as if it passed.
    let (files, errors) = measure_all(f.paths());
    assert!(errors.is_empty(), "{errors:?}");
    let physical = LibraryRollup::measure(&files, RollupOptions::default());
    let apparent = LibraryRollup::measure(&files, RollupOptions::for_basis(SizeBasis::Apparent));

    assert!(
        !physical.within(per_inode, 1.0),
        "if the physical rollup now matches du -sb, the bases collapsed and \
         this ticket's central distinction is gone (physical={})",
        physical.total
    );
    assert!(
        physical.total > apparent.total,
        "block overhead is positive"
    );
}

#[test]
fn a_hardlinked_library_reports_reclaimable_bytes() {
    // The #4409 shape: several stash ids kept, one copy on disk.
    let f = Fixture::new();
    let (files, errors) = measure_all(f.paths());
    assert!(errors.is_empty(), "{errors:?}");
    let physical = LibraryRollup::measure(&files, RollupOptions::default());

    assert_eq!(physical.file_count, 32, "30 small + whole + whole_link");
    assert_eq!(
        physical.distinct_inodes, 31,
        "the two whole_* paths are one inode"
    );
    assert_eq!(physical.hardlink_extra_paths, 1);
    assert_eq!(
        physical.hardlink_savings, 4096,
        "one 4 KiB link is reclaimable"
    );
    assert!(
        physical.summary().contains("reclaimable"),
        "{}",
        physical.summary()
    );
}

#[test]
fn an_unreadable_path_is_reported_not_skipped() {
    let f = Fixture::new();
    let mut paths = f.paths();
    paths.push(f.dir.path().join("not-here"));
    let err: Vec<SizeError> = LibraryRollup::from_paths(&paths, RollupOptions::default())
        .expect_err("a missing file must not silently vanish from the total");
    assert_eq!(err.len(), 1);
    assert!(err[0].to_string().contains("not-here"), "{}", err[0]);

    // With the opt-in, the total is still right for what *was* readable, and
    // the skipped path is named.
    let lenient = RollupOptions {
        skip_unreadable: true,
        ..RollupOptions::default()
    };
    let r = LibraryRollup::from_paths(&paths, lenient).unwrap();
    assert_eq!(r.file_count, 32);
    assert_eq!(r.unreadable.len(), 1);
}

#[test]
fn the_temp_root_is_outside_generated_and_actually_writable() {
    // The #5646 requirement is that a transcode can run when `generated/` is
    // not writable, so the temp root has to be somewhere that is. Prove it by
    // making generated/ read-only and still producing a job dir.
    let f = Fixture::new();
    let library = f.dir.path().join("library");
    let generated = library.join("generated");
    std::fs::create_dir_all(&generated).unwrap();
    let temp_elsewhere = f.dir.path().join("scratch");

    // generated/ is on the same filesystem and is read-only.
    set_mode(&generated, 0o500);

    let tmp = TempRoot::new(&temp_elsewhere).unwrap();
    let job = tmp.job_dir("transcode", "scene 1").unwrap();
    assert!(
        job.exists(),
        "the job dir is outside generated/, so it is writable"
    );
    std::fs::write(job.join("frame-0001.webp"), b"pixels").unwrap();
    assert_eq!(std::fs::read(job.join("frame-0001.webp")).unwrap().len(), 6);

    // And configuring the temp root *into* generated/ is refused outright.
    let bad = TempRoot::new(generated.join("temp")).unwrap_err();
    assert!(bad.contains("5646"), "{bad}");

    // Restore permissions so the temp dir can be cleaned up.
    set_mode(&generated, 0o700);
}

#[test]
fn disk_space_agrees_with_df() {
    // statvfs is the implementation; df is the reference a user will run.
    let space = disk_space(Path::new("/")).unwrap();
    let out = Command::new("df").arg("-B1").arg("/").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1).expect("df prints a header and a row");
    let cols: Vec<&str> = line.split_whitespace().collect();
    let df_total: u64 = cols[1].parse().unwrap();
    let df_avail: u64 = cols[3].parse().unwrap();

    // df's total counts the same blocks, and nothing frees or allocates a block
    // of the filesystem's size, so the two agree exactly.
    assert_eq!(space.total, df_total, "statvfs total vs df total");

    // The available column is f_bavail -- the unprivileged figure, which is what
    // this module reports as `available` precisely so a user is not told they can
    // write blocks reserved for root -- but it is a *live counter*, sampled
    // twice at two different instants with a process and the test harness
    // writing in between. It agrees to within a few blocks, not exactly, and
    // demanding exactness makes this test fail whenever the machine is busy.
    //
    // The bound is generous because it exists to catch a real regression: a
    // mis-mapped column, or reporting f_bfree where f_bavail was meant, differs
    // by the root reserve, which is hundreds of megabytes on a real filesystem.
    let drift = space.available.abs_diff(df_avail);
    assert!(
        drift <= 16 * 4096,
        "statvfs bavail vs df available drifted by {drift} bytes, which is more \
         than concurrent writes between the two samples should account for"
    );
    assert!(
        human(space.available).ends_with("iB") || human(space.available).ends_with(" B"),
        "{}",
        human(space.available)
    );
}
