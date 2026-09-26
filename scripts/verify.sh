#!/usr/bin/env bash
# Full-workspace verification for the Commons repo.
#
# --no-fail-fast is not optional. `cargo test --workspace` stops at the first
# failing target, so a green run above a broken test is a green run that has
# not looked. See handoff section 9.
set -uo pipefail

REPO="${REPO:-/home/alvaro/code-local/rust/commons}"
export COMMONS_FFMPEG="${COMMONS_FFMPEG:-$HOME/.hermes/tools/ffmpeg-9.0.1-linux-x64/bin/ffmpeg}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target/commons}"

cd "$REPO" || exit 1

fail=0

echo "== cargo fmt --all --check"
if cargo fmt --all --check; then
  echo "   ok"
else
  echo "   FAILED"
  fail=1
fi

echo "== cargo clippy --workspace --all-targets -- -D warnings"
clippy_out=$(cargo clippy --workspace --all-targets -- -D warnings 2>&1)
if [ -z "$(printf '%s' "$clippy_out" | grep -E '^(error|warning)')" ]; then
  echo "   ok"
else
  printf '%s\n' "$clippy_out" | grep -E '^(error|warning)' -A 4 | head -20
  echo "   FAILED"
  fail=1
fi

echo "== cargo test --workspace --no-fail-fast"
test_out=$(cargo test --workspace --no-fail-fast 2>&1)
printf '%s' "$test_out" | grep -E 'test result:' > /tmp/commons-test-results.txt 2>/dev/null
pass=$(grep -cE 'test result: ok' /tmp/commons-test-results.txt 2>/dev/null || echo 0)
tot_p=$(awk '{for(i=1;i<=NF;i++) if($i=="passed;"){split($(i-1),a,";");s+=a[1]}} END{print s+0}' /tmp/commons-test-results.txt)
tot_f=$(awk '{for(i=1;i<=NF;i++) if($i=="failed;"){split($(i-1),a,";");s+=a[1]}} END{print s+0}' /tmp/commons-test-results.txt)
echo "   targets ok: $pass, tests passed: $tot_p, tests failed: $tot_f"
if [ "$tot_f" -ne 0 ]; then
  printf '%s' "$test_out" | grep -E '^---- .* stdout' -A 6 | head -60
  echo "   FAILED"
  fail=1
fi

# The plan's own bookkeeping. A ticket marked done whose file is not there is
# the expensive failure -- it reads as progress and builds as nothing -- so that
# is a gate. An unwritten future ticket is normal and only reported.
echo "== plan status"
if python3 "$REPO/scripts/plan-status.py" > /tmp/commons-plan.txt 2>&1; then
  sed -n '3,6p' /tmp/commons-plan.txt | sed 's/^/   /'
else
  sed -n '/CLAIMED DONE/,$p' /tmp/commons-plan.txt | sed 's/^/   /'
  echo "   FAILED: the plan claims a ticket is done and its file is absent"
  fail=1
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "ALL GREEN"
else
  echo "NOT GREEN"
fi
exit $fail
