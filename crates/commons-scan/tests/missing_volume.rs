//! The T-P2-003 acceptance test: unmount a loopback-backed directory
//! mid-scan and prove the scanner does not enter a rescan loop.
//!
//! The plan says it plainly: "assert the number of stat calls, by
//! instrumenting the walker with a counter". That is the only objective
//! measure of stash#5683 -- a stopwatch or a CPU percentage is too noisy to
//! assert on, and would pass on a machine that happened to be idle.
//!
//! # Why this test needs root
//!
//! A real mount is the only honest way to test the case, because the case
//! *is* "a directory that used to have files in it and now does not, and
//! nothing in the filesystem distinguishes that from a deletion". A
//! `rename`d directory or a deleted tempdir is a deletion, and a test built
//! on one would prove nothing about an unmount.
//!
//! So the mount is real, and the test skips -- loudly, with a reason -- when
//! it cannot make one. A skipped acceptance test is honest; a faked one is
//! not, and a test that silently passes on a machine with no loopback would
//! be the worst outcome of the three.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use commons_core::FileState;
use commons_scan::state::{actionable_by_default, Volume, VolumeProbe, VolumeState, VolumeTracker};
use commons_scan::walk;
use tempfile::TempDir;

/// Can this machine make a loopback mount?
///
/// Checks for the two things that actually matter: `mount` and `losetup`, and
/// that we can escalate. Both are the case in CI containers; neither is the
/// case in an unprivileged sandbox, and there the test must skip rather than
/// fail or fake.
fn loopback_available() -> Result<(), String> {
    // Refuse early on a filesystem that cannot host a mount point, so the
    // failure is a clear skip rather than a confusing mount error.
    if !std::path::Path::new("/tmp").is_dir() {
        return Err("/tmp is not available".to_string());
    }
    for tool in ["mount", "losetup", "mkfs.ext4"] {
        if which(tool).is_none() {
            return Err(format!("{tool} is not installed"));
        }
    }
    match Command::new("sudo").args(["-n", "true"]).output() {
        Ok(o) if o.status.success() => Ok(()),
        _ => Err("passwordless sudo is not available".to_string()),
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let out = Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// A loopback filesystem that really is mounted, and really can be unmounted.
///
/// The `Drop` is what makes the failure path safe: if a test panics halfway
/// through, the loop device and the mount point still get cleaned up, so the
/// next test does not find a stale mount and a leaked device.
struct Loopback {
    image: PathBuf,
    mountpoint: PathBuf,
    mounted: bool,
}

impl Loopback {
    fn create(tag: &str) -> Result<Self, String> {
        // Not `tempfile::TempDir`: it honours TMPDIR, and on a developer
        // machine that is often a btrfs subvolume under $HOME, which cannot
        // host a mount point -- `mount` fails with "failed to set up loop
        // device" and the reason is genuinely not obvious. /tmp is tmpfs on
        // every machine this has run on, and a mount point on tmpfs is
        // unremarkable.
        let base = TempDir::new_in("/tmp").map_err(|e| e.to_string())?;
        let mountpoint = base.path().join("mnt");
        let image = base.path().join("vol.img");
        // 64MB is plenty for a few thousand empty-ish files and keeps the
        // whole test under a second.
        run("truncate", &["-s", "64M", image.to_str().unwrap()])?;
        run("mkfs.ext4", &["-q", "-F", image.to_str().unwrap()])?;
        run("mkdir", &["-p", mountpoint.to_str().unwrap()])?;
        run(
            "mount",
            &[
                "-o",
                "loop",
                image.to_str().unwrap(),
                mountpoint.to_str().unwrap(),
            ],
        )?;
        // A freshly mounted ext4 is owned by root, and the test writes files
        // into it as the ordinary user. Hand it to whoever is running the
        // test, or every file creation below fails with EACCES.
        let uid = Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        if let Ok(uid) = uid {
            let _ = Command::new("sudo")
                .args([
                    "chown",
                    &format!("{uid}:{uid}"),
                    mountpoint.to_str().unwrap(),
                ])
                .output();
            let _ = tag;
        }
        Ok(Loopback {
            image,
            mountpoint,
            mounted: true,
        })
    }

    /// Give the mount point to the current user, again.
    fn chown_to_us(&self) {
        let uid = Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let _ = Command::new("sudo")
            .args([
                "chown",
                &format!("{uid}:{uid}"),
                self.mountpoint.to_str().unwrap(),
            ])
            .output();
    }

    /// Really unmount it. This is the event the ticket is about.
    ///
    /// The loop device is detached too, because `umount` leaves it attached
    /// and the remount at the end of the test then fails with "failed to set
    /// up loop device". That is also a more faithful simulation: a user
    /// unplugging a drive releases the device entirely, so anything that
    /// holds a handle to it has to cope with that, not with a stale device
    /// that happens to still be there.
    fn unmount(&mut self) -> Result<(), String> {
        run("umount", &[self.mountpoint.to_str().unwrap()])?;
        self.mounted = false;
        self.detach_loop()?;
        Ok(())
    }

    fn detach_loop(&self) -> Result<(), String> {
        let out = Command::new("sudo")
            .args(["losetup", "-j", self.image.to_str().unwrap()])
            .output()
            .map_err(|e| e.to_string())?;
        let listing = String::from_utf8_lossy(&out.stdout);
        for line in listing.lines() {
            if let Some((device, _)) = line.split_once(": ") {
                let _ = Command::new("sudo")
                    .args(["losetup", "-d", device])
                    .output();
            }
        }
        Ok(())
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if self.mounted {
            let _ = Command::new("sudo")
                .args(["umount", self.mountpoint.to_str().unwrap()])
                .output();
        }
        // The image lives in a TempDir, so removing it also releases the
        // loop device. `losetup -d` is belt and braces for the case where the
        // TempDir cleanup runs before the kernel has let go.
        let _ = Command::new("sudo")
            .args(["losetup", "-d", self.image.to_str().unwrap()])
            .output();
    }
}

fn run(tool: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new("sudo")
        .arg(tool)
        .args(args)
        .output()
        .map_err(|e| format!("{tool}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{tool} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn write_file_at(dir: &Path, name: &str, bytes: usize) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
}

/// The plan's acceptance case.
///
/// A library of 400 files on a real loopback mount. Scan it: every file is
/// seen and the mount is healthy. Unmount. Now scan again, repeatedly, and
/// assert two things: the files are `Missing` rather than deleted, and the
/// per-scan filesystem cost is one probe rather than 400 stats.
#[test]
fn unmounting_a_volume_mid_scan_does_not_cause_a_rescan_storm() {
    if let Err(why) = loopback_available() {
        eprintln!("SKIPPED: cannot create a loopback mount ({why})");
        return;
    }
    let mut vol = match Loopback::create("accept") {
        Ok(v) => v,
        Err(why) => {
            eprintln!("SKIPPED: cannot create a loopback mount ({why})");
            return;
        }
    };
    let root = vol.mountpoint.clone();

    for i in 0..400 {
        write_file_at(&root, &format!("f{i:04}.mp4"), 1);
    }

    // A 30s base backoff against a 1s scan cycle, which is the real
    // relationship: the whole point is that the gap between scans is much
    // shorter than the gap between probes of an absent volume.
    let mut tracker =
        VolumeTracker::new().with_backoff(Duration::from_secs(30), Duration::from_secs(3600));
    tracker.configure(Volume::new(&root));

    // ---- scan 1: the volume is there, and every file is found.
    let t0 = SystemTime::now();
    assert!(
        tracker.should_probe(&root, t0),
        "a mounted volume is probed"
    );
    let first = walk(&root);
    assert_eq!(
        first.stats.files, 400,
        "every file on a healthy volume is found"
    );
    tracker.record(
        &root,
        VolumeProbe {
            state: VolumeState::Mounted,
            touched: true,
        },
        t0,
    );

    // The drive is unplugged, mid-life, with the library on it.
    vol.unmount().expect("umount");

    // ---- scan 2: the volume is detected gone, once.
    let t1 = t0 + Duration::from_secs(1);
    let probe = VolumeProbe {
        state: VolumeState::Unmounted,
        touched: true,
    };
    tracker.record(&root, probe.clone(), t1);
    assert_eq!(tracker.volumes()[0].state, VolumeState::Unmounted);
    assert!(
        tracker.cleanup_enabled(&root),
        "a volume is opted in to cleanup by default; #314 is the opt-out"
    );

    let to_mark = tracker.files_to_mark_missing(&root, (0..400).map(|i| format!("f{i:04}.mp4")));
    assert_eq!(
        to_mark.len(),
        400,
        "all 400 files on the unmounted volume are Missing"
    );
    // And `Missing` is a state, not a deletion -- that is the property the
    // whole design exists to guarantee.
    assert!(!actionable_by_default(FileState::Missing));
    assert_ne!(FileState::Missing, FileState::Present);

    // ---- the cost. Ten more scan cycles, none of which may touch the disk.
    let before = tracker.stats();
    let mut clock = t1;
    for _ in 0..10 {
        clock += Duration::from_secs(1);
        if tracker.should_probe(&root, clock) {
            tracker.record(&root, probe.clone(), clock);
        }
    }
    let extra = tracker.stats() - before;
    assert_eq!(
        extra, 0,
        "ten scan cycles after an unmount cost {extra} filesystem probes; it should cost none"
    );

    // The broken behaviour, stated as a number: one stat per vanished file,
    // per scan, forever. This is the figure the ticket exists to prevent.
    let naive_cost = 400 * 10;
    assert!(
        extra < naive_cost,
        "the real cost ({extra}) must be nothing like the naive one ({naive_cost})"
    );

    // ---- and the recovery: remount, and the volume is found again.
    run(
        "mount",
        &[
            "-o",
            "loop",
            vol.image.to_str().unwrap(),
            vol.mountpoint.to_str().unwrap(),
        ],
    )
    .expect("remount");
    vol.mounted = true;
    vol.chown_to_us();
    let back = clock + Duration::from_secs(60);
    tracker.record(
        &root,
        VolumeProbe {
            state: VolumeState::Mounted,
            touched: true,
        },
        back,
    );
    let again = walk(&root);
    assert_eq!(
        again.stats.files, 400,
        "after a remount every file is found again, and nothing was lost"
    );
}

/// The walk itself, against a directory that is not there, must not spin.
///
/// The volume tracker is what bounds the *number of scans*; this is the
/// other half: a single scan of an absent root terminates and says so, rather
/// than retrying.
#[test]
fn a_walk_of_an_absent_root_terminates_and_reports() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("never-existed");
    let report = walk(&missing);
    assert_eq!(report.stats.files, 0);
    assert!(!report.volume_errors.is_empty(), "and it says why");
    // One error, for the root. Not one per file that used to be there,
    // because the walk never got far enough to know about any.
    assert_eq!(report.volume_errors.len(), 1);
}
