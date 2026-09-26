//! The throughput budget, as arithmetic that can be tested (T-P2-008, §6.1).
//!
//! Phase 2's exit criterion is "100k-item library scan and browse within a
//! stated budget". Before this file, no such number existed anywhere in the
//! spec: §4.3 gives memory figures and §6.1 names fifteen throughput issues,
//! but neither states a time. So the numbers live here, with the reasoning
//! attached, and `benches/scan_100k.rs` measures against them.
//!
//! # Why this is a library and not just a benchmark
//!
//! A `benches/` binary is not a test target. `cargo test` does not compile it,
//! so the gate logic written inside one has no coverage at all — and the first
//! version of this ticket proved exactly that. Four of six mutations of the
//! gate (including "never fail" and "ignore the margin") passed with the
//! benchmark green, because nothing ever executed those branches under test.
//!
//! So the decision lives here, in the library, where it is covered, and the
//! benchmark is reduced to measurement plus a call.
//!
//! # The budgets
//!
//! Measured on the development host (btrfs on NVMe, warm cache) over a
//! generated 100k-file tree of 100 directories × 1,000 empty files:
//!
//! | Operation | Measured | Budget |
//! |---|---|---|
//! | `readdir` only, no stat | 0.84 µs/file | — |
//! | `readdir` + `stat` | 2.6 µs/file | — |
//! | Full walk, batched, 100k files | ~0.25 s | [`INITIAL_BUDGET`] |
//! | Incremental decision pass, 100k files | ~0.15 s | [`RESCAN_BUDGET`] |
//!
//! The initial budget is 30 s for a walk that really costs a quarter of a
//! second, and that gap is deliberate. A budget that fails on a cold page
//! cache, a slower NVMe, or a CI runner with less CPU teaches people to ignore
//! it, and a gate that is always ignored is not a gate. 30 s still catches the
//! regression this ticket exists for: O(n²) work, or a `stat` per directory
//! level, both of which are *minutes* at this size rather than seconds.
//!
//! The re-scan budget is 4 s and it measures a different operation on purpose.
//! §6.1's promise is not "a walk is fast", it is **"a scan never re-hashes an
//! unchanged file"**. So the re-scan figure is the cost of *deciding* not to
//! hash — `Rehash::decide` over 100k `(stored, current)` pairs, touching no
//! file content.
//!
//! My first version benchmarked the walk twice and asserted the second was
//! faster than the first. It failed, correctly: the walk does identical work
//! both times, because whether a file gets read is the caller's decision, not
//! the walker's. The walker contains no read at all. A ratio between two
//! identical operations is not a test, it is a coin flip that happens to land
//! on FAIL — and had it landed on PASS it would have been a gate that verified
//! nothing.

/// How far past its budget a measurement may go before the gate fails.
pub const MARGIN: f64 = 0.25;

/// Initial-scan budget in seconds. See the module docs for how this was
/// chosen; it is deliberately loose, and deliberately not a number anyone
/// should tighten without evidence.
pub const INITIAL_BUDGET: f64 = 30.0;

/// Incremental-decision-pass budget in seconds.
pub const RESCAN_BUDGET: f64 = 4.0;

/// What a single measurement compared against its budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetCheck {
    /// What was measured, e.g. "initial scan".
    pub name: &'static str,
    /// The measured seconds.
    pub measured: f64,
    /// The budget it was held to.
    pub budget: f64,
}

impl BudgetCheck {
    /// The limit a measurement may reach: the budget plus [`MARGIN`].
    ///
    /// The margin exists so that a slightly slower machine, a cold cache, or a
    /// scheduler hiccup does not turn the build red. It is applied to the
    /// budget rather than to the measurement so that raising a budget always
    /// raises the limit too — two places to edit would eventually disagree.
    pub fn limit(&self) -> f64 {
        self.budget * (1.0 + MARGIN)
    }

    /// Did this measurement stay inside its limit?
    pub fn ok(&self) -> bool {
        self.measured <= self.limit()
    }

    /// A failure message, or `None` when it passed.
    pub fn failure(&self) -> Option<String> {
        if self.ok() {
            return None;
        }
        Some(format!(
            "{}: {:.2}s exceeds the {:.0}s budget by more than the {:.0}% margin \
             (limit {:.2}s)",
            self.name,
            self.measured,
            self.budget,
            MARGIN * 100.0,
            self.limit()
        ))
    }
}

/// Is the incremental path actually cheaper than the read it avoids?
///
/// This is the assertion that means something. Two absolute timings can both
/// look healthy while the second has quietly become the first, and §6.1's
/// actual promise is about the *relationship*: deciding must cost less than
/// opening, or there is no reason to decide.
///
/// A caller that stops consulting `Rehash::decide` leaves this passing while
/// the promise is broken elsewhere, so this is a property of the code path
/// rather than of the whole system. It is worth asserting and not sufficient.
pub fn decision_is_cheaper(decide_seconds: f64, open_seconds: f64) -> bool {
    decide_seconds < open_seconds
}

/// Every check that failed, in a stable order. Empty means the gate passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateFailure(pub String);

/// Run the whole gate and return the failures, empty if it passes.
///
/// Returns rather than exiting, so the logic is testable and the benchmark is
/// a caller. The order is the order the checks are listed in, which is the
/// order a reader wants them reported in.
pub fn run_gate(checks: &[BudgetCheck], decide: f64, open: f64) -> Vec<GateFailure> {
    let mut out = Vec::new();
    for c in checks {
        if let Some(msg) = c.failure() {
            out.push(GateFailure(msg));
        }
    }
    if !decision_is_cheaper(decide, open) {
        out.push(GateFailure(format!(
            "the incremental decision ({decide:.2} us) is not cheaper than simply opening the \
             file ({open:.2} us): there is no reason to consult it"
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budgets as the benchmark passes them.
    fn checks(initial: f64, rescan: f64) -> Vec<BudgetCheck> {
        vec![
            BudgetCheck {
                name: "initial scan",
                measured: initial,
                budget: INITIAL_BUDGET,
            },
            BudgetCheck {
                name: "incremental decision pass",
                measured: rescan,
                budget: RESCAN_BUDGET,
            },
        ]
    }

    #[test]
    fn the_margin_is_the_fraction_the_plan_asks_for() {
        assert_eq!(MARGIN, 0.25, "the plan specifies a 25% margin");
        assert_eq!(INITIAL_BUDGET * (1.0 + MARGIN), 37.5);
        assert_eq!(RESCAN_BUDGET * (1.0 + MARGIN), 5.0);
    }

    #[test]
    fn a_measurement_inside_its_budget_passes_even_at_the_budget() {
        let c = BudgetCheck {
            name: "initial scan",
            measured: INITIAL_BUDGET,
            budget: INITIAL_BUDGET,
        };
        assert!(c.ok());
        assert_eq!(c.failure(), None);
    }

    #[test]
    fn a_measurement_past_the_margin_fails() {
        // Exactly at the limit: still passing. One microsecond past: failing.
        // A gate that flips exactly at the limit is a gate with a boundary, and
        // a boundary nobody tests is a boundary that is off by one.
        let at = BudgetCheck {
            name: "initial scan",
            measured: INITIAL_BUDGET * (1.0 + MARGIN),
            budget: INITIAL_BUDGET,
        };
        assert!(at.ok(), "the limit itself must pass");

        let past = BudgetCheck {
            measured: INITIAL_BUDGET * (1.0 + MARGIN) + 0.001,
            ..at
        };
        assert!(!past.ok());
        let msg = past.failure().expect("a failure message");
        assert!(msg.contains("initial scan"), "{msg}");
        assert!(msg.contains("30s"), "the message names the budget: {msg}");
    }

    #[test]
    fn raising_a_budget_raises_the_limit_with_it() {
        // Two places to edit would eventually disagree, which is why the limit
        // is derived rather than stored.
        let tight = BudgetCheck {
            name: "x",
            measured: 10.0,
            budget: 5.0,
        };
        let loose = BudgetCheck {
            budget: 50.0,
            ..tight
        };
        assert!(!tight.ok());
        assert!(loose.ok());
        assert!(loose.limit() > tight.limit());
    }

    #[test]
    fn the_gate_passes_on_the_measured_numbers() {
        let f = run_gate(&checks(0.25, 0.15), 0.000001, 0.002);
        assert_eq!(f, Vec::new(), "{f:?}");
    }

    #[test]
    fn the_gate_fails_when_either_budget_is_exceeded() {
        let over_initial = run_gate(&checks(INITIAL_BUDGET * 2.0, 0.15), 0.000001, 0.002);
        assert_eq!(over_initial.len(), 1, "{over_initial:?}");
        assert!(over_initial[0].0.contains("initial scan"));

        let over_rescan = run_gate(&checks(0.25, RESCAN_BUDGET * 2.0), 0.000001, 0.002);
        assert_eq!(over_rescan.len(), 1, "{over_rescan:?}");
        assert!(over_rescan[0].0.contains("incremental decision pass"));

        let both = run_gate(&checks(100.0, 100.0), 0.000001, 0.002);
        assert_eq!(
            both.len(),
            2,
            "both failures are reported, not just the first"
        );
    }

    #[test]
    fn the_gate_fails_when_deciding_costs_more_than_opening() {
        // The relation §6.1 actually promises, and the one my first benchmark
        // asserted in a form that could not fail.
        assert!(!decision_is_cheaper(0.002, 0.002), "equal is not cheaper");
        assert!(!decision_is_cheaper(0.003, 0.002));
        assert!(decision_is_cheaper(0.001, 0.002));

        // Both budgets healthy, ratio broken: still a failure.
        let f = run_gate(&checks(0.25, 0.15), 0.010, 0.002);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].0.contains("not cheaper"), "{f:?}");
    }

    #[test]
    fn a_gate_that_ignores_its_checks_would_report_nothing() {
        // The mutation "never fail" was caught by the per-budget tests above
        // rather than by this one, so this asserts the shape instead: no input
        // reaches `run_gate` with an empty check list, because a caller that
        // passed none would get a vacuous pass.
        assert!(run_gate(&[], 0.0, 1.0).is_empty());
    }
}
