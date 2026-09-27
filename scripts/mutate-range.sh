#!/usr/bin/env bash
# Mutation-check the two places the wire's inclusive range becomes the internal
# half-open one.
#
# The whole type exists because `bytes=0-99` is inclusive and everything
# downstream wants a subtraction. Those two conversions are one `+ 1` and one
# `- 1`, in two functions, and a wrong value in either is a body one byte short
# or a read one byte past the end of the file. Neither shows up in a compile
# error; `u64` is `u64` either way.
set -u
cd /home/alvaro/code-local/rust/commons || exit 1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target/commons}"

SRC=crates/commons-server/src/range.rs
BACKUP=$(mktemp)
cp "$SRC" "$BACKUP"

# `commons-server`'s build script or its test harness wants a reachable
# DATABASE_URL even for a test that touches no database, so without one every
# mutation reports "did not compile" -- which reads as a clean run.
export DATABASE_URL="${DATABASE_URL:-postgres://postgres:${COMMONS_PGPW:-smoke_pw}@127.0.0.1/postgres}"

# The label is "all tests pass", which is what a SURVIVOR is -- and it is worth
# saying so, because "baseline: killed" would be a nonsense first line and
# "baseline: green" is clearer than a word that only means something in
# contrast to the lines below it.
verdict() {
  local out
  out=$(cargo test -p commons-server --test range 2>&1)
  # The order here matters and the first version got it backwards. `error:` is
  # matched BEFORE the test result, and cargo prints `error: test failed, to
  # rerun pass ...` at the end of every FAILED run -- so a killed mutant was
  # reported as "did not compile" and all seven read as no-ops. The compile
  # check is anchored to `^error[E` (the E is rustc's error code) and `^error:`
  # with a bracket, which is what a real type error looks like, and the test
  # result is read FIRST anyway.
  if printf '%s' "$out" | grep -qE 'test result: FAILED'; then
    echo "killed"
  elif printf '%s' "$out" | grep -qE 'test result: ok'; then
    echo "all tests pass"
  elif printf '%s' "$out" | grep -qE '^error\[E'; then
    echo "no result (did not compile)"
  else
    echo "no result"
  fi
}

apply() {
  local label="$1"
  cp "$BACKUP" "$SRC"
  if ! python3 - "$SRC" <<PY
import sys
p = sys.argv[1]
s = open(p).read()
before = s
$2
if s == before:
    print("MUTATION DID NOT APPLY -- the source moved, fix the script")
    sys.exit(3)
open(p, "w").write(s)
PY
  then
    echo "$label: COULD NOT APPLY"
    return
  fi
  echo "$label: $(verdict)"
}

echo "baseline (all pass = nothing to kill yet): $(verdict)"
echo

# The whole reason ByteRange is half-open: the wire's inclusive end plus one.
apply "inclusive -> half-open conversion dropped (the + 1)" \
"s = s.replace('requested.min(len - 1) + 1', 'requested.min(len - 1)')"

# And its inverse. Written as `end` rather than `end - 1`, this makes every 206
# claim one byte more than it sends.
apply "half-open -> inclusive dropped (the - 1 in content_range)" \
"s = s.replace('range.start, range.end - 1, len', 'range.start, range.end, len')"

# The suffix branch, which is the one place that did not have the `+ 1` at
# first. It is half-open by construction (`end: len`), so a mutation here is
# the one an inclusive-minded rewrite makes.
apply "suffix range end made inclusive" \
"s = s.replace('return Ok(RangeSpec::One(ByteRange { start, end: len }));', 'return Ok(RangeSpec::One(ByteRange { start, end: len - 1 }));')"

# Clamping -> rejecting. `0-999999` on a 500-byte file is the whole file per
# 14.1.2; a parser that 416s it breaks every client that does not know the
# length, and the tests that only ever ask for in-bounds ranges will not notice.
apply "an end past the body is rejected instead of clamped" \
"s = s.replace('requested.min(len - 1) + 1', '(if requested >= len { return Err(RangeError::Unsatisfiable); } else { requested + 1 })')"

# The `len == 0` guard, which looks redundant because the start check below
# also rejects everything on an empty body -- except that a SUFFIX range on an
# empty body computes start = 0 and would otherwise be accepted.
apply "the empty-body guard removed" \
"s = s.replace('    if len == 0 {\n        return Err(RangeError::Unsatisfiable);\n    }\n', '')"

# Malformed vs Unsatisfiable collapsed. They are both 416 to a browser and
# different to a client, and the distinction is the whole reason they are two
# variants.
apply "malformed and unsatisfiable collapsed into one error" \
"s = s.replace('Err(RangeError::Malformed)', 'Err(RangeError::Unsatisfiable)')"

# The open-ended range stops at the last byte rather than being half-open.
apply "an open-ended range ends one byte early" \
"s = s.replace('    let end: u64 = if last.is_empty() {\n        len\n    } else {', '    let end: u64 = if last.is_empty() {\n        len - 1\n    } else {')"

cp "$BACKUP" "$SRC"
echo
echo "restored: $(verdict)"
rm -f "$BACKUP"
