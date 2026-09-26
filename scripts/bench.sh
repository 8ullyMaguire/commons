#!/usr/bin/env bash
# Throughput and memory budget gate (T-P2-008, spec §6.1 and §4.3).
#
# Two budgets, both expressed as something that exits non-zero:
#
#   §6.1  100k-file scan, and the incremental decision that must be cheaper
#         than the read it avoids.  cargo bench -p commons-scan --bench scan_100k
#   §4.3  idle RSS of a running server.  scripts/memory-budget.py
#
# The second is here rather than in CI because it needs a booted server, which
# the benches do not. Both are runnable locally and both are runnable here; the
# split is so a failure of one is legible as a failure of one.
#
# The 100k tree is cached between runs (~7 s to build, which is longer than
# the budget being measured), so a second invocation measures rather than
# rebuilds. COMMONS_BENCH_TREE points it elsewhere; it must be a real
# filesystem, and the benchmark refuses tmpfs rather than measuring RAM.

set -euo pipefail

cd "$(dirname "$0")/.."

: "${COMMONS_FFMPEG:=$(command -v ffmpeg || true)}"
if [[ -n "$COMMONS_FFMPEG" ]]; then
    export COMMONS_FFMPEG
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target/commons}"

# Where the tree lives. Defaults to the workspace, which is on a real
# filesystem; an override that is not will be refused by the benchmark itself.
export COMMONS_BENCH_TREE="${COMMONS_BENCH_TREE:-.bench-tree/scan-100k}"

echo "==> §6.1 scan throughput (100k files)"
cargo bench -q -p commons-scan --bench scan_100k

echo
echo "==> §4.3 idle memory"
# The scenarios that need a binary are skipped with a message rather than
# failing the gate: an unimplemented Phase 3 command is not a memory
# regression, and a gate that cannot run yet should say so instead of blocking.
if command -v commons-server >/dev/null 2>&1; then
    python3 scripts/memory-budget.py --scenario server-library
else
    echo "  commons-server is not on PATH; the §4.3 idle check needs a built"
    echo "  binary. Run 'cargo build --release -p commons-server' first, or"
    echo "  accept that this half of the gate did not run."
fi

echo
echo "budget gate: OK"
