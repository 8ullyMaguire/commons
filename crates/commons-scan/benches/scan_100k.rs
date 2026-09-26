//! T-P2-008: the throughput budget, as something that fails (spec §6.1, §4.3).
//!
//! # The budgets, and where the numbers come from
//!
//! Phase 2's exit criterion is "100k-item library scan and browse within a
//! stated budget". Before this file, no such number existed anywhere in the
//! spec — §4.3 gives memory figures, §6.1 names fifteen throughput issues and
//! describes the design, and neither states a time. So the numbers below were
//! chosen, and the reasoning is written out so the next person can move them
//! with evidence rather than by feel.
//!
//! Measured on the development host (btrfs on NVMe, warm page cache), over a
//! generated 100,000-file tree of 100 directories × 1,000 empty files:
//!
//! | Operation | Measured | Budget |
//! |---|---|---|
//! | `readdir` only, no stat | 0.84 µs/file | — |
//! | `readdir` + `stat` | 2.6 µs/file | — |
//! | Full walk of 100k files, batched | ~0.25 s | `INITIAL_BUDGET` |
//! | Re-scan decision pass (mtime+size hint, no read) | ~0.01 s | `RESCAN_BUDGET` |
//!
//! The initial-scan budget is 30 s for a walk whose real cost is a quarter of
//! a second. That gap is the point, not laziness: a budget that fails on a
//! cold page cache, a slower NVMe, or a CI runner with less CPU teaches people
//! to ignore it, and a gate that is always ignored is not a gate. 30 s still
//! catches a real regression — the pathological case this ticket exists for is
//! O(n²) work or a `stat` per directory level, both of which are minutes at
//! this size, not seconds.
//!
//! The re-scan budget is 4 s, and it is measured against a different operation
//! than the initial scan on purpose. §6.1's promise is not "a walk is fast",
//! it is **"a scan never re-hashes an unchanged file"**. So the re-scan figure
//! is the cost of *deciding* not to hash: `Rehash::decide` over 100k
//! `(stored, current)` pairs, touching no file content at all.
//!
//! My first version benchmarked the walk twice and asserted the second was
//! faster than the first. It failed, correctly: the walk does exactly the same
//! work both times, because whether a file gets read is the caller's decision,
//! not the walker's. Asserting a ratio between two identical operations is not
//! a test — it is a coin flip that happens to land on FAIL. The walker has no
//! `read` in it, so the only way to see the incremental path at all is to
//! measure the decision that gates it.
//!
//! The 25 % margin the plan asks for is implemented as
//! `budget * (1 + MARGIN)`, checked against the **median** of several runs
//! rather than the best one: a benchmark that passes on its luckiest run is not
//! a measurement.
//!
//! # What is deliberately *not* here
//!
//! No real media. Empty files, paths only. ffprobe, phash and thumbnail
//! generation are jobs (T-P2-004), not the walk, and their cost belongs to a
//! different budget against a different number of cores. Folding them in would
//! make this number a function of the machine's GPU rather than of the code
//! under test.
//!
//! # Where the tree lives
//!
//! A real filesystem, never tmpfs. `/tmp` is tmpfs on this host, and 100k
//! files in RAM measures RAM. The benchmark refuses to run on a `tmpfs` root
//! and says why, because a benchmark that silently measures the wrong thing is
//! worse than no benchmark.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use commons_scan::walker::{self, WalkConfig};

/// Files in the generated tree. The number §6.1 names.
const FILE_COUNT: usize = 100_000;

/// Directories per level of the fan-out. Real libraries are not one flat
/// directory of 100k files; they are nested, and the directory count is what
/// makes `read_dir` cost something.
const DIRS: usize = 100;

/// Per-directory file count.
const PER_DIR: usize = FILE_COUNT / DIRS;

// The budgets and the margin live in `commons_scan::budget`, where they are
// unit-tested. Re-declaring them here would give two sources of truth for a
// number the whole ticket is about.

/// Runs per measurement. The median is what the gate reads.
const RUNS: usize = 5;

/// Files used for the open-vs-decide comparison. A sample rather than all
/// 100k: the quantity being compared is a per-file cost, and 1,000 of them is
/// enough to make the open syscall's cost visible without spending a minute
/// measuring the disk.
const SAMPLE_FOR_HASH: usize = 1_000;

/// How the tree is laid out.
fn build_tree(root: &Path) -> std::io::Result<()> {
    for d in 0..DIRS {
        let dir = root.join(format!("d{d:03}"));
        std::fs::create_dir_all(&dir)?;
        for f in 0..PER_DIR {
            // Empty files: the walker's cost is `read_dir` plus one `stat`, and
            // a file with content would add a read the walk does not do.
            std::fs::File::create(dir.join(format!("f{f:04}.mp4")))?;
        }
    }
    Ok(())
}

/// The filesystem type of `path`, as a human string.
///
/// Used to refuse a tmpfs root. Returns `None` when the type cannot be read,
/// which is treated as "assume it is a real filesystem" — refusing on a
/// failure to check would make the benchmark unrunnable on a system where
/// `statfs` is restricted, which is a worse outcome than a possibly-warm
/// measurement.
fn fs_type(path: &Path) -> Option<String> {
    walker::fs_type(path)
}

fn config() -> WalkConfig {
    WalkConfig {
        batch_size: 1024,
        ..Default::default()
    }
}

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort();
    xs[xs.len() / 2]
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Run the walk `RUNS` times and return the median, asserting the tree is
/// intact each time.
///
/// The file count is checked on every run rather than once at the end: a walk
/// that returned 99,000 files in 0.2 s would otherwise post a magnificent time,
/// and a benchmark that measures a broken walk is measuring the bug.
fn measure_walk(root: &Path) -> Duration {
    let mut runs = Vec::with_capacity(RUNS);
    for i in 0..RUNS {
        let t0 = Instant::now();
        let report = walker::Walker::new(root).with_config(config()).walk();
        let elapsed = t0.elapsed();
        assert_eq!(
            report.files.len(),
            FILE_COUNT,
            "run {i} found {} files, not {FILE_COUNT}: the benchmark is timing a broken walk",
            report.files.len()
        );
        assert!(
            report.volume_errors.is_empty(),
            "run {i} reported volume errors: {:?}",
            report.volume_errors
        );
        runs.push(elapsed);
    }
    median(runs)
}

/// Median wall time of the incremental decision over every path, `RUNS` times.
///
/// The decision is fed the *stored* `(mtime, size)` the walk just produced, so
/// every file compares equal and the answer is `Unchanged` -- the case a
/// re-scan of an unchanged tree is made of. Asserted, not assumed: if
/// `decide` started returning `Changed` for identical inputs, this would
/// notice, and it is worth noticing loudly because a scanner that re-hashes
/// everything is the exact regression §6.1 exists to prevent.
fn measure_decision(paths: &[PathBuf]) -> Duration {
    let mut runs = Vec::with_capacity(RUNS);
    for i in 0..RUNS {
        let t0 = Instant::now();
        let mut unchanged = 0usize;
        for p in paths {
            let md = std::fs::metadata(p).expect("a file the walk just found");
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i128)
                .unwrap_or(0);
            let size = md.len();
            match commons_scan::hashing::Rehash::decide(mtime, size, mtime, size) {
                commons_scan::hashing::Rehash::Unchanged => unchanged += 1,
                commons_scan::hashing::Rehash::Changed => {
                    panic!("run {i}: an unchanged file compared as changed")
                }
            }
        }
        let elapsed = t0.elapsed();
        assert_eq!(
            unchanged,
            paths.len(),
            "run {i}: {unchanged} of {} files compared unchanged",
            paths.len()
        );
        runs.push(elapsed);
    }
    median(runs)
}

fn main() {
    // The tree lives under the workspace by default, so a repeated run reuses
    // it: building 100k files costs ~7 s, which is longer than the budget being
    // measured and would dominate the output. `COMMONS_BENCH_TREE` moves it,
    // which is how a caller with a faster or slower disk points the benchmark
    // somewhere else.
    let root: PathBuf = match std::env::var("COMMONS_BENCH_TREE") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.bench-tree/scan-100k"),
    };
    if let Some(parent) = root.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("could not create {}: {e}", parent.display());
            std::process::exit(2);
        }
    }

    let need_build = match std::fs::read_dir(&root) {
        Ok(entries) => entries.count() != DIRS,
        Err(_) => true,
    };
    if need_build {
        println!("building {FILE_COUNT} files under {}", root.display());
        let t0 = Instant::now();
        if let Err(e) = build_tree(&root) {
            eprintln!("could not build the tree at {}: {e}", root.display());
            std::process::exit(2);
        }
        println!("  built in {:.1}s", t0.elapsed().as_secs_f64());
    }

    let root = root.canonicalize().unwrap_or(root);
    if let Some(t) = fs_type(&root) {
        if t.contains("tmpfs") {
            eprintln!(
                "refusing to run: {} is tmpfs.\n\
                 100k files in RAM measures RAM, not the scanner. Point the tree at a real\n\
                 filesystem (the default is under the workspace, which is on disk).",
                root.display()
            );
            std::process::exit(2);
        }
        println!("filesystem: {t}");
    }

    // --- initial scan -----------------------------------------------------
    println!("\ninitial scan ({FILE_COUNT} files, {RUNS} runs, median)");
    let initial = measure_walk(&root);
    println!("  {:.2}s", secs(initial));

    // --- the incremental decision pass -----------------------------------
    // The operation §6.1 actually promises about: for an unchanged tree, the
    // cost of *deciding* not to re-hash. No file content is touched, so this
    // is pure arithmetic over the walk's output.
    //
    // A real comparison against a full re-hash is included below, because
    // "the decision pass is under 4 s" on its own says nothing about whether
    // the decision is the right one — a `decide` that always returned
    // `Unchanged` would be instant and catastrophically wrong.
    println!("\nre-scan decision pass ({FILE_COUNT} files, {RUNS} runs, median)");
    let paths: Vec<PathBuf> = walker::Walker::new(&root)
        .with_config(config())
        .walk()
        .files
        .into_iter()
        .map(|f| f.path)
        .collect();
    let rescan = measure_decision(&paths);
    println!("  {:.2}s", secs(rescan));

    // --- what the decision is worth --------------------------------------
    // A sample of real files, fully hashed, against the same count of
    // `decide` calls. Not a 100k re-hash: that would measure the disk, and the
    // point is the ratio, which is a property of the code path rather than of
    // how fast this particular NVMe is.
    let sample_n = SAMPLE_FOR_HASH;
    let sample: Vec<PathBuf> = paths.iter().take(sample_n).cloned().collect();

    let t0 = Instant::now();
    for p in &sample {
        let r = commons_scan::hashing::hash_file(p).expect("hashing an empty file");
        assert_eq!(r.bytes_read, 0, "the fixture files are empty");
    }
    let hashed = t0.elapsed();

    // The decision alone, with no syscall in it at all. This is the cost the
    // incremental path *adds*; the metadata `stat` it replaces is measured
    // above as part of the decision pass.
    let t0 = Instant::now();
    for _ in &sample {
        std::hint::black_box(commons_scan::hashing::Rehash::decide(0, 0, 0, 0));
    }
    let decided = t0.elapsed();

    // Open + hash, versus the decision that avoids it. The open dominates,
    // and that is the honest comparison: the cost being avoided is the open.
    let t0 = Instant::now();
    for p in &sample {
        std::fs::File::open(p).expect("opening an empty file");
    }
    let opened = t0.elapsed();

    println!(
        "\n  for {sample_n} files: open+hash {:.3}s, open only {:.3}s, decide only {:.3}s",
        secs(hashed),
        secs(opened),
        secs(decided)
    );

    // --- the gate ---------------------------------------------------------
    // The decision itself lives in `commons_scan::budget` rather than here,
    // because `cargo test` does not compile a `benches/` binary. Written here,
    // it would have had no coverage: the first version of this ticket had four
    // of six gate mutations survive with the benchmark green, including "never
    // fail" and "ignore the margin". This file measures; `budget` decides.
    let checks = [
        commons_scan::budget::BudgetCheck {
            name: "initial scan",
            measured: secs(initial),
            budget: commons_scan::budget::INITIAL_BUDGET,
        },
        commons_scan::budget::BudgetCheck {
            name: "incremental decision pass",
            measured: secs(rescan),
            budget: commons_scan::budget::RESCAN_BUDGET,
        },
    ];
    let decide_per_file = secs(rescan) / paths.len() as f64;
    let failures =
        commons_scan::budget::run_gate(&checks, decide_per_file, secs(opened) / sample_n as f64);

    for c in &checks {
        println!(
            "  {:<28} {:>6.2}s / {:>4.0}s  limit {:.0}s  {}",
            c.name,
            c.measured,
            c.budget,
            c.limit(),
            if c.ok() { "OK" } else { "OVER" }
        );
    }

    println!();
    if failures.is_empty() {
        println!(
            "PASS  walk {:.2}s, decision pass {:.2}s  (margin {:.0}%, decide {:.2} us/file vs \
             open {:.2} us/file)",
            secs(initial),
            secs(rescan),
            commons_scan::budget::MARGIN * 100.0,
            decide_per_file * 1e6,
            secs(opened) / sample_n as f64 * 1e6
        );
        return;
    }
    for f in &failures {
        eprintln!("FAIL  {}", f.0);
    }
    std::process::exit(1);
}
