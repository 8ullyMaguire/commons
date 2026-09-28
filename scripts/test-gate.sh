#!/bin/sh
# The workspace test gate, and the two ways a naive one lies.
#
# Usage:  scripts/test-gate.sh [log-file ...]
#         cargo test --workspace > /tmp/ws1.log 2>&1
#         scripts/test-gate.sh /tmp/ws1.log /tmp/ws2.log
#
# With no arguments it runs the suite itself, twice, and gates both runs.
#
# WHY THIS IS A SCRIPT AND NOT A ONE-LINE PIPELINE
# ================================================
#
# The obvious gate is
#
#     cargo test --workspace 2>&1 | grep -E "^test .* FAILED|panicked at"
#     cargo test --workspace 2>&1 | grep -E "^test result" | awk '{...}'
#
# and it passes on a run that tested nothing. Both halves, independently:
#
#   1. A run that dies during the BUILD prints no `test result` lines. The awk
#      aggregates zero rows and prints `passed= failed= ignored= suites=`,
#      which reads as an oddly clean zero rather than as a missing measurement.
#      The FAILED grep also returns 0, because there is nothing to fail.
#
#      Observed for real: a baseline worktree whose CARGO_TARGET_DIR was under
#      /tmp filled a 16G tmpfs, and the run died with
#      `No space left on device (os error 28)` during linking, `exit=101`, and
#      ZERO test result lines. The gate said "0 failures". It had run no tests.
#
#   2. A run that DOES produce results can still have FAILED lines in it while
#      the totals reconcile perfectly. Observed for real: one run printed
#      `passed=1884 failed=0 ignored=1 suites=106` and, in the same output, two
#      lines reading FAILED. The count is computed from the very run that
#      failed, so it cannot notice.
#
# So the gate is three assertions, not one, and the first is "did anything
# run at all". A gate that cannot distinguish "clean" from "unmeasured" will
# certify an unmeasured build, and the empty aggregate is the shape that gets
# believed.
#
# WHY TWICE
# =========
#
# A flake at ~2% per spawn appears in roughly one run in three. One green run
# is not evidence that a flake is fixed; it is evidence that it did not fire.
# Two runs is the cheapest thing that separates "fixed" from "not observed yet".

set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$REPO_ROOT"

# /tmp on this host is a 16G tmpfs and this workspace's target/ is ~86G. Pointing
# CARGO_TARGET_DIR there is how the baseline run died; a second target dir is a
# second full build of ~40 crates and a full RAM bill. Override if you must.
: "${CARGO_TARGET_DIR:=/home/alvaro/.cargo-target/commons}"
export CARGO_TARGET_DIR
: "${DATABASE_URL:=postgres://postgres:smoke_pw@127.0.0.1/postgres}"
export DATABASE_URL
: "${PGHOST:=127.0.0.1}"
: "${PGUSER:=postgres}"
: "${PGPASSWORD:=smoke_pw}"
export PGHOST PGUSER PGPASSWORD

LOGDIR=${LOGDIR:-/tmp}

gate_one() {
  log=$1
  label=$2

  suites=$(grep -cE '^test result' "$log" || true)
  if [ "$suites" -eq 0 ]; then
    echo "GATE FAIL [$label]: no 'test result' lines in $log"
    echo "  The run produced no results. The build probably died first."
    grep -E '^error|No space left|could not compile|linking with' "$log" 2>/dev/null | head -5 || true
    return 1
  fi

  # A suite count far below the known baseline means the run aborted partway,
  # which is the same "unmeasured" failure wearing a plausible number.
  if [ "$suites" -lt 100 ]; then
    echo "GATE FAIL [$label]: only $suites suites reported; a full run is ~108."
    return 1
  fi

  if grep -qE '^test .* FAILED|panicked at' "$log"; then
    echo "GATE FAIL [$label]: FAILED lines present (the totals may still look clean)"
    grep -nE '^test .* FAILED|panicked at' "$log" | head -10
    return 1
  fi

  summary=$(grep -E '^test result' "$log" \
    | awk '{p+=$4; f+=$6; i+=$8; s++} END {printf "passed=%d failed=%d ignored=%d suites=%d", p, f, i, s}')
  echo "GATE PASS [$label]: $summary"
  echo "$summary" > "$LOGDIR/.gate-summary-$label"
  return 0
}

status=0

if [ "$#" -gt 0 ]; then
  # Gate logs the caller already produced.
  n=0
  for log in "$@"; do
    n=$((n + 1))
    gate_one "$log" "run$n" || status=1
  done
  if [ "$n" -lt 2 ]; then
    echo "GATE WARN: gated $n run(s). Two are needed to tell a fixed flake from an unobserved one."
  fi
  exit "$status"
fi

# No logs given: run the suite ourselves, twice.
for n in 1 2; do
  log="$LOGDIR/commons-gate-run$n.log"
  echo "=== run $n -> $log"
  if ! cargo test --workspace > "$log" 2>&1; then
    echo "cargo test exited non-zero (run $n); the log is the evidence."
  fi
  gate_one "$log" "run$n" || status=1
done

if [ "$status" -eq 0 ]; then
  echo
  echo "GATE: both runs clean."
  echo "  baseline at a238cca: passed=1887 failed=0 ignored=1 suites=107"
  echo "  Any change to that number must be explained by a file count, not by"
  echo "  adjusting the expectation. Count the tests you added and reconcile."
fi

exit "$status"
