//! A scratch directory that deletes itself.
//!
//! # Why this exists
//!
//! The three test files that need a working directory used to create one and
//! leave it. There were 3710 of them under `$TMPDIR` after a few hundred suite
//! runs, 917 MB, and the disk they lived on reached 100% — at which point
//! unrelated tests started failing with `ETXTBSY` and
//! `No such file or directory` while `exec`ing their fixtures. The suite was
//! reporting a disk-full symptom as a dozen unrelated flaky tests, which is the
//! worst way for a test failure to arrive.
//!
//! # Why a guard and not a teardown call
//!
//! Because the leak is worst exactly where cleanup is skipped. Every explicit
//! `remove_dir_all` has to come after the work, so it is skipped by an early
//! return, a `?`, a panic, or a test that simply ends without reaching it — and
//! the runs that fail are the runs that leak. `Drop` runs on all four paths,
//! including an unwind from a failed assertion.
//!
//! Cleanup is best-effort: a failure to delete is ignored rather than
//! panicking in a `Drop`, where a second panic during an unwind aborts the
//! process and takes the real failure message with it.

use std::path::{Path, PathBuf};

/// A working directory that is removed when this value is dropped.
///
/// Derefs to the path, so a caller writes `let dir = scratch("ass");` and then
/// `dir.join("m.mkv")` — no `.path()` at every use site.
pub struct Scratch {
    path: PathBuf,
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A fresh directory named after `tag`, removed when the handle is dropped.
///
/// Unique per call, per process and across runs, for the reasons the callers
/// already document — pid reuse, two calls in the same nanosecond on different
/// cores. All three keys are needed and none is sufficient alone.
pub fn scratch(tag: &str) -> Scratch {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "commons-test-{tag}-{}-{n}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    // Wipe any same-named directory from a previous run before use, so a stale
    // file cannot make a test pass once and fail forever after.
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch dir");
    Scratch { path }
}

/// Write a fixture file, returning its path.
///
/// `#[allow(dead_code)]` because each test file is a separate crate including
/// this one, and only one of the three uses it.
#[allow(dead_code)]
pub fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).expect("write fixture");
    p
}
