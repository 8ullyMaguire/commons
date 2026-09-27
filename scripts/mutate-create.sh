#!/usr/bin/env bash
# Mutation-check object creation.
#
# The claims worth attacking are the ones a caller cannot see break: an id that
# covers a field that changes, a create that publishes itself, a create that
# writes no consent row, and an action that reports what it wrote rather than
# what it found. Each is a claim in the module docs, so each is a mutation here.
#
# # Why the mutations are separate files
#
# The first version inlined them as python snippets in an unquoted shell
# heredoc, and three things went wrong at once: bash expanded `$` and backticks,
# a single-line `s.replace` stopped matching the moment `cargo fmt` reflowed the
# line, and a mutant that did not compile reported the same way as a survivor.
# So: one file per mutation, each a plain python script taking the target path,
# each ending in an `assert` naming the text it could not find.
set -u
cd /home/alvaro/code-local/rust/commons || exit 1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target/commons}"
export DATABASE_URL="${DATABASE_URL:-postgres://postgres:***@127.0.0.1/postgres}"

SRC=crates/commons-store/src/create.rs
MUT=scripts/mutations
BACKUP=$(mktemp)
cp "$SRC" "$BACKUP"

# The result is read BEFORE the compile check: cargo prints `error: test failed`
# at the end of every failed run, so checking for compilation first reports
# every killed mutant as malformed.
verdict() {
  local out
  out=$(cargo test -p commons-store --test create_db 2>&1)
  if printf '%s' "$out" | grep -qE 'test result: FAILED'; then
    echo "killed"
  elif printf '%s' "$out" | grep -qE 'test result: ok'; then
    echo "*** SURVIVED ***"
  elif printf '%s' "$out" | grep -qE '^error(\[|:)'; then
    echo "DID NOT COMPILE -- the mutation is malformed, not a survivor"
  else
    echo "no result; tail:"
    printf '%s\n' "$out" | tail -6
  fi
}

run_one() {
  local script="$1"
  local label="$2"
  cp "$BACKUP" "$SRC"
  if ! python3 "$MUT/$script" "$SRC"; then
    echo "$label: COULD NOT APPLY"
    return
  fi
  echo "$label: $(verdict)"
}

echo "baseline, source UNMUTATED -- want green: $(verdict)"
echo

# --- the id ---------------------------------------------------------------
run_one create_separator_is_comma.py    "field boundaries are unambiguous"
run_one create_id_covers_organized.py   "the id ignores fields that change"

# --- consent --------------------------------------------------------------
run_one create_skips_consent.py        "a created object is visible to its creator"
run_one create_publishes_itself.py     "a create does not publish itself"

# --- the counts -----------------------------------------------------------
run_one create_lies_about_already_there.py "created and existing are counted separately"

cp "$BACKUP" "$SRC"
echo
echo "restored, source UNMUTATED -- want green: $(verdict)"
rm -f "$BACKUP"

# Note on the baseline line: a green run there is the DESIRED state, and the
# verdict function's wording ("SURVIVED") is aimed at a mutated run, so the two
# lines are labelled for what they are rather than left to be misread.
