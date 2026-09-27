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

# The two-engine tests (T-P5-001's search parity) need a reachable Postgres.
# Set here rather than left to the caller's environment because a parity test
# that runs against one engine is not a parity test: §3.5 makes the second
# engine a property of the product, not a deployment choice. Overridable, and
# the value below is the local development server -- no credential belongs in
# this file for a machine other than this one, so a deployment that differs
# exports its own.
export DATABASE_URL="${DATABASE_URL:-postgres://postgres:smoke_pw@127.0.0.1/postgres}"

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

# The dedup mutation pass, opt-in. Twelve behaviour-changing mutations in the
# classifier and the relation store, each required to turn a named test red. It
# rebuilds the workspace once per mutation, so it is a flag rather than part of
# the default gate -- but a number in the README that nobody ever recomputes is
# a number that rots, and the two mutations it found were both in code the
# ordinary suite called green.
if [ "${COMMONS_MUTATE:-0}" = "1" ]; then
  # Every mutation script, not just the first one. Six features now have a
  # mutation pass, and a gate that only runs the oldest is a gate that has
  # quietly stopped covering most of the code it claims to.
  #
  # The list is explicit rather than globbed: a glob over `scripts/mutate-*.py`
  # would pick up a scratch script and, worse, would silently stop covering a
  # feature when its script is renamed. A missing entry is a visible edit here.
  for mut in dedup bulk; do
    script="$REPO/scripts/mutate-$mut.py"
    [ -f "$script" ] || { echo "   FAILED: $script is missing"; fail=1; continue; }
    echo "== mutation pass ($mut)"
    if python3 "$script" > "/tmp/commons-mutate-$mut.txt" 2>&1; then
      tail -1 "/tmp/commons-mutate-$mut.txt" | sed 's/^/   /'
    else
      sed -n '/survived:/,$p' "/tmp/commons-mutate-$mut.txt" | sed 's/^/   /'
      echo "   FAILED: a mutation survived, so a test is not testing what it claims"
      fail=1
    fi
    echo
  done
  # The UI scripts target TypeScript and drive the node harness, so they run
  # from `ui/`; `mutate-dedup.py` and `mutate-bulk.py` target Rust and its
  # `#[cfg(test)]` suites, so they run from the repo root. Same loop, different
  # working directory, named per script rather than guessed from the filename.
  for mut in bulk-ui gestures guard-ui selection commands-ui; do
    script="$REPO/scripts/mutate-$mut.py"
    [ -f "$script" ] || { echo "   FAILED: $script is missing"; fail=1; continue; }
    echo "== mutation pass ($mut, ui)"
    if (cd "$REPO/ui" && python3 "$script" > "/tmp/commons-mutate-$mut.txt" 2>&1); then
      # The summary line, not the last line. `mutate-commands-ui.py` ends with a
      # per-mutant explanation of its known-equivalent mutants, so `tail -1`
      # printed a long parenthetical in the middle of an otherwise green log --
      # which reads as a failure to anyone skimming, and is the kind of thing
      # that trains people to ignore this section.
      grep -E '^[0-9]+/[0-9]+ killed$' "/tmp/commons-mutate-$mut.txt" | sed 's/^/   /'
    else
      sed -n '/real survivors/,$p' "/tmp/commons-mutate-$mut.txt" | sed 's/^/   /'
      echo "   FAILED: a mutation survived, so a test is not testing what it claims"
      fail=1
    fi
    echo
  done
fi

# The history scan is a *publishing* gate, not a build gate: it reads every
# blob ever committed, which is slow and only matters before something leaves
# the machine. It is a flag for the same reason the mutation pass is.
if [ "${COMMONS_SCAN_SECRETS:-0}" = "1" ]; then
  echo "== history secret scan"
  if python3 "$REPO/scripts/scan-history-secrets.py" | sed 's/^/   /'; then
    :
  else
    echo "   FAILED: a credential-shaped string is in the history"
    fail=1
  fi
  echo "== scanner self-test (can it still fail?)"
  if python3 "$REPO/scripts/scan-history-secrets-selftest.py" | tail -1 | sed 's/^/   /'; then
    :
  else
    echo "   FAILED: a pattern the scanner claims to check no longer fires"
    fail=1
  fi
  echo
fi

# The UI. Both halves, and the build between them.
#
# The build is not optional before the e2e: these are static routes served from
# `build/`, and a stale build serves an *older* bundle that still returns HTTP
# 200 for every route. A green e2e run against a stale bundle proves nothing
# about the code on disk, which is the single most expensive way to lose an
# afternoon here.
if [ "${COMMONS_SKIP_UI:-0}" != "1" ] && [ -d "$REPO/ui/node_modules" ]; then
  echo "== ui: build"
  if (cd "$REPO/ui" && node node_modules/vite/bin/vite.js build) > /tmp/commons-ui-build.txt 2>&1; then
    echo "   ok"
  else
    tail -20 /tmp/commons-ui-build.txt | sed 's/^/   /'
    echo "   FAILED: the UI did not build"
    fail=1
  fi

# Every scripts/mutate-*.mjs must at least PARSE. Three of them shipped with a
# syntax error (an apostrophe inside a single-quoted note: string, and a
# multi-line pattern inside one), and the symptom reads as the mutation script
# finding nothing rather than the script never running. One line here makes that
# class impossible to miss.
echo "== mutation scripts parse"
for m in "$REPO"/scripts/mutate-*.mjs; do
  [ -e "$m" ] || continue
  if node --check "$m" >/dev/null 2>&1; then
    echo "   ok: $(basename "$m")"
  else
    echo "   PARSE ERROR: $(basename "$m")"
    node --check "$m" 2>&1 | head -4 | sed 's/^/      /'
    fail=1
  fi
done

echo "== ui: unit tests"
  if (cd "$REPO/ui" && node ./tests/run-tests.mjs) > /tmp/commons-ui-unit.txt 2>&1; then
    grep -E '^. (tests|pass|fail) ' /tmp/commons-ui-unit.txt | sed 's/^/   /'
  else
    grep -E '^. (tests|pass|fail) |not ok' /tmp/commons-ui-unit.txt | head -20 | sed 's/^/   /'
    echo "   FAILED: a UI unit test failed"
    fail=1
  fi

  echo "== ui: end-to-end"
  if (cd "$REPO/ui" && node_modules/.bin/playwright test) > /tmp/commons-ui-e2e.txt 2>&1; then
    grep -E 'passed' /tmp/commons-ui-e2e.txt | tail -1 | sed 's/^/   /'
  else
    grep -E '✘|failed|passed' /tmp/commons-ui-e2e.txt | head -20 | sed 's/^/   /'
    echo "   FAILED: a Playwright test failed"
    fail=1
  fi
  echo
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
