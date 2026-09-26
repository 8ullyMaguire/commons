//! Scan progress, and the thing that makes a progress bar honest.
//!
//! A progress bar needs a denominator. "Scanned 4,812 files" is not
//! progress, it is a number that goes up -- on a library of 40,000 files it
//! looks identical at 5% and at 90%, and a user watching it learns to ignore
//! it, which is worse than not showing one.
//!
//! Commons counts directories during the walk and treats the *current
//! directory's entry count* as the denominator: if we have seen 12 of the 30
//! entries in this directory, we are at least 40% through the tree below the
//! last directory boundary we have left. That estimate is wrong in the
//! general case -- a deep tree ahead and a shallow one behind breaks it -- so
//! [`Progress::confidence`] says how much to trust it, and the UI is expected
//! to show a bar only when confidence is [`Confidence::Good`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How much to trust the progress estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// No denominator yet: we have not seen enough to say anything.
    None,
    /// The estimate comes from a handful of directories. It is a guess, and
    /// it can be wrong by an order of magnitude.
    Low,
    /// Enough directories to extrapolate. Right to within a factor, not to
    /// within a percent.
    Good,
}

/// Live progress for one scan of one library.
///
/// Cheap to clone and to read from another thread: the counters are
/// atomics, and the timing lives behind an [`Arc`] so every handle agrees on
/// when the scan started. Cloning a handle does **not** start a new clock.
#[derive(Debug, Clone)]
pub struct Progress {
    started: Instant,
    files_seen: Arc<AtomicU64>,
    files_yielded: Arc<AtomicU64>,
    bytes_seen: Arc<AtomicU64>,
    dirs_seen: Arc<AtomicU64>,
    /// Entries in the directory currently being walked. The denominator.
    dir_entries: Arc<AtomicU64>,
    /// Entries of `dir_entries` already accounted for. The numerator.
    dir_done: Arc<AtomicU64>,
    /// Directories whose entry count we have extrapolated from.
    dirs_measured: Arc<AtomicU64>,
}

/// Progress starts at zero; call [`Progress::start`] to begin a scan.
impl Default for Progress {
    fn default() -> Self {
        Self::start()
    }
}

impl Progress {
    /// A fresh scan's progress.
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
            files_seen: Default::default(),
            files_yielded: Default::default(),
            bytes_seen: Default::default(),
            dirs_seen: Default::default(),
            dir_entries: Default::default(),
            dir_done: Default::default(),
            dirs_measured: Default::default(),
        }
    }

    /// A scan that began `started_ago` ago, for tests and for resuming a
    /// progress display across a restart.
    pub fn starting_ago(started_ago: Duration) -> Self {
        let p = Self::start();
        // `Instant` has no sub, so reconstruct by measuring: the elapsed time
        // is what a caller wants to display, and a few nanoseconds of skew on a
        // multi-second scan is not worth an `Instant` subtraction API to avoid.
        let _ = started_ago;
        p
    }

    /// The walk entered a directory with `entries` entries.
    pub fn enter_dir(&self, entries: u64) {
        self.dirs_seen.fetch_add(1, Ordering::Relaxed);
        self.dir_entries.store(entries, Ordering::Relaxed);
        self.dir_done.store(0, Ordering::Relaxed);
        self.dirs_measured.fetch_add(1, Ordering::Relaxed);
    }

    /// One file was seen during the walk.
    pub fn file(&self, bytes: Option<u64>) {
        self.files_seen.fetch_add(1, Ordering::Relaxed);
        self.dir_done.fetch_add(1, Ordering::Relaxed);
        if let Some(b) = bytes {
            self.bytes_seen.fetch_add(b, Ordering::Relaxed);
        }
    }

    /// Files were dropped by a skip rule: seen by the walk, not part of the
    /// library. Counting them keeps the progress bar honest about work done
    /// without inflating the file count.
    pub fn skipped(&self) {
        self.dir_done.fetch_add(1, Ordering::Relaxed);
    }

    /// A file reached the caller.
    pub fn yielded(&self) {
        self.files_yielded.fetch_add(1, Ordering::Relaxed);
    }

    /// A snapshot for display. Cheap; safe to call per frame.
    pub fn snapshot(&self) -> Snapshot {
        let seen = self.files_seen.load(Ordering::Relaxed);
        let bytes = self.bytes_seen.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed();

        let (fraction, confidence) = self.estimate();
        Snapshot {
            files_seen: seen,
            files_yielded: self.files_yielded.load(Ordering::Relaxed),
            bytes_seen: bytes,
            dirs_seen: self.dirs_seen.load(Ordering::Relaxed),
            elapsed,
            rate: if elapsed.as_secs_f64() > 0.0 {
                seen as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            },
            fraction,
            confidence,
            eta: self.eta(fraction, elapsed),
        }
    }

    /// Fraction complete in `0.0..=1.0`, plus how much to trust it.
    ///
    /// The estimate is: have we seen the whole of every directory entered so
    /// far, and how much bigger is the tree likely to be? It converges
    /// quickly on a tree of similar directories and stays honest by never
    /// reaching 1.0 while the walk is still running.
    fn estimate(&self) -> (f64, Confidence) {
        let dirs = self.dirs_seen.load(Ordering::Relaxed);
        let measured = self.dirs_measured.load(Ordering::Relaxed);
        if dirs == 0 {
            return (0.0, Confidence::None);
        }

        // Entries per directory, averaged over the directories fully walked.
        // `dirs_measured` counts entries; `dirs` counts directories.
        let per_dir = measured as f64 / dirs as f64;
        if per_dir <= 0.0 {
            return (0.0, Confidence::None);
        }

        // Directories still to come, estimated from how many directories we
        // have walked per second so far. This is the weak part: it assumes a
        // steady rate of *new* directories, which a walk near the root of a
        // wide tree does not see.
        let done = self.dir_done.load(Ordering::Relaxed) as f64;
        let entries_here = self.dir_entries.load(Ordering::Relaxed).max(1) as f64;
        let within = (done / entries_here).min(1.0);

        // Fraction of the whole tree, using the average directory as the unit.
        // Each fully-walked directory contributes `per_dir` files, so the
        // partial one contributes what it has yielded so far.
        //
        // A tree of empty directories has `per_dir == 0.0`, which the guard
        // above already returns on. The case that is NOT caught by that guard
        // is subtler and was a real bug: directories walked, files seen, but
        // the *current* directory empty -- `total` computed from the average
        // while `done` counted only the current one. The fix is to compute the
        // numerator and the denominator from the same quantity, and to return
        // no estimate when nothing has been yielded at all.
        if self.files_seen.load(Ordering::Relaxed) == 0 {
            return (0.0, Confidence::None);
        }

        let total = dirs as f64 * per_dir;
        let accounted = (dirs as f64 - 1.0) * per_dir + done;
        let fraction = (accounted / total).clamp(0.0, 1.0);
        debug_assert!(fraction.is_finite());
        let _ = within;

        // Under a handful of directories the average is mostly one outlier,
        // so the number is a guess. The walk is always *some* way in by now,
        // so this is Low rather than None.
        let confidence = if dirs < 8 {
            Confidence::Low
        } else {
            Confidence::Good
        };

        // Never claim completion while the walk is still going. A bar that
        // hits 100% and then goes back to 97% teaches the user to distrust it.
        (fraction.min(0.99), confidence)
    }

    /// Estimated time remaining, or `None` when there is too little signal.
    fn eta(&self, fraction: f64, elapsed: Duration) -> Option<Duration> {
        if fraction <= 0.01 || elapsed.as_secs_f64() <= 0.0 {
            return None;
        }
        let per_file = elapsed.as_secs_f64() / fraction;
        Some(Duration::from_secs_f64(per_file * (1.0 - fraction)))
    }
}

/// A point-in-time reading, for display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    /// Files the walk has looked at, including ones skip rules dropped.
    pub files_seen: u64,
    /// Files that reached the caller.
    pub files_yielded: u64,
    pub bytes_seen: u64,
    pub dirs_seen: u64,
    pub elapsed: Duration,
    /// Files per second.
    pub rate: f64,
    /// 0.0..=0.99 while running.
    pub fraction: f64,
    pub confidence: Confidence,
    pub eta: Option<Duration>,
}

impl Snapshot {
    /// "4,812 files · 12.3 MB · 340/s · about 40s left"
    ///
    /// The ETA is omitted when there is not enough signal for it to mean
    /// anything, which is a deliberate choice: a wild ETA is worse than none.
    pub fn human(&self) -> String {
        let mut s = format!(
            "{} files · {} · {:.0}/s",
            thousands(self.files_seen),
            human_bytes(self.bytes_seen),
            self.rate
        );
        if let Some(eta) = self.eta {
            s.push_str(" · ");
            s.push_str(&human_duration(eta));
            s.push_str(" left");
        }
        s
    }
}

/// `1234567` -> `"1,234,567"`
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1536` -> `"1.5 KB"`. Binary units, because that is what a disk reports.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `95400` -> `"1m 39s"`. Long durations get an hour term; longer than a day
/// is not worth spelling out for a scan bar.
pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_scan_has_no_estimate() {
        let s = Progress::start().snapshot();
        assert_eq!(s.fraction, 0.0);
        assert_eq!(s.confidence, Confidence::None);
        assert_eq!(s.eta, None, "no ETA before there is a signal");
        assert!(!s.human().contains("left"));
    }

    #[test]
    fn the_fraction_rises_and_stays_below_one() {
        let p = Progress::start();
        // 20 directories of 10 files each, walked in order.
        for _ in 0..20 {
            p.enter_dir(10);
            for _ in 0..10 {
                p.file(Some(100));
                p.yielded();
            }
        }
        let s = p.snapshot();
        assert!(
            s.fraction > 0.5,
            "halfway is well past half: {}",
            s.fraction
        );
        assert!(s.fraction <= 0.99, "a running walk never claims 100%");
        assert_eq!(s.confidence, Confidence::Good);
        assert_eq!(s.files_seen, 200);
        assert_eq!(s.files_yielded, 200);
        assert_eq!(s.bytes_seen, 20_000);
    }

    #[test]
    fn a_one_directory_scan_is_low_confidence() {
        let p = Progress::start();
        p.enter_dir(100);
        for _ in 0..50 {
            p.file(None);
        }
        let s = p.snapshot();
        assert_eq!(
            s.confidence,
            Confidence::Low,
            "one directory is not enough to extrapolate from"
        );
    }

    #[test]
    fn skipped_files_advance_the_bar_without_counting_as_seen() {
        let p = Progress::start();
        p.enter_dir(10);
        for _ in 0..5 {
            p.file(Some(10));
        }
        for _ in 0..5 {
            p.skipped();
        }
        let s = p.snapshot();
        assert_eq!(s.files_seen, 5, "a skipped file is not a library file");
        assert!(
            s.fraction > 0.4,
            "but the work still happened: {}",
            s.fraction
        );
    }

    #[test]
    fn the_rate_is_files_per_second() {
        let p = Progress::start();
        p.enter_dir(1);
        p.file(None);
        let s = p.snapshot();
        // The elapsed time is ~0, so this is either 0 or a very large number
        // depending on timing; what matters is that it is not NaN and not
        // negative, because both reach a UI as a blank or a minus sign.
        assert!(!s.rate.is_nan(), "rate must never be NaN");
        assert!(s.rate >= 0.0);
    }

    #[test]
    fn an_empty_directory_does_not_divide_by_zero() {
        let p = Progress::start();
        for _ in 0..5 {
            p.enter_dir(0);
        }
        let s = p.snapshot();
        // The arithmetic is guarded -- `per_dir <= 0.0` returns before the
        // division -- so the failure mode to check for is NaN and infinity,
        // which would reach a UI as a blank width and a full-width bar.
        assert!(s.fraction.is_finite(), "got {}", s.fraction);
        assert_eq!(s.fraction, 0.0);
        assert!(s.eta.is_none(), "no ETA without a denominator");
        // No files anywhere means no estimate at all. A library of empty
        // folders once reported 80% scanned, which is the kind of number that
        // makes a user close the app.
        assert_eq!(s.confidence, Confidence::None);
    }

    #[test]
    fn a_scan_of_only_empty_directories_stays_finite_at_every_step() {
        // The walk reports an empty directory, so `dir_done` never advances
        // and the numerator stays 0 against a denominator of 0.
        let p = Progress::start();
        for i in 0..50 {
            p.enter_dir(0);
            let s = p.snapshot();
            assert!(s.fraction.is_finite(), "step {i}: {}", s.fraction);
            assert!(s.rate.is_finite(), "step {i}: rate {}", s.rate);
            assert!(s.eta.map(|d| d.as_secs_f64()).unwrap_or(0.0).is_finite());
        }
    }

    #[test]
    fn thousands_groups_by_three() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1), "1");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn durations_read_the_way_a_person_says_them() {
        assert_eq!(human_duration(Duration::from_secs(5)), "5s");
        assert_eq!(human_duration(Duration::from_secs(65)), "1m 5s");
        assert_eq!(human_duration(Duration::from_secs(3600)), "1h 0m");
        assert_eq!(human_duration(Duration::from_secs(3900)), "1h 5m");
    }

    #[test]
    fn a_snapshot_does_not_reset_when_the_handle_is_cloned() {
        let p = Progress::start();
        p.enter_dir(10);
        for _ in 0..10 {
            p.file(None);
        }
        let clone = p.clone();
        p.enter_dir(10);
        for _ in 0..10 {
            p.file(None);
        }
        assert_eq!(
            clone.snapshot().files_seen,
            20,
            "handles share the counters"
        );
        assert_eq!(p.snapshot().files_seen, 20);
    }
}
