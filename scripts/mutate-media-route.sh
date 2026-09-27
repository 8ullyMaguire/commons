#!/usr/bin/env bash
# Mutation-check the media route.
#
# The route's claims are the ones that matter: consent before bytes, 404 rather
# than 403, and a body that matches its own Content-Range. A route test that
# passes over a route that serves denied content is worse than no test, because
# it is a green tick on the security boundary.
#
# # Why the mutations are separate files
#
# The first three versions inlined each mutation as a python snippet in a shell
# heredoc, and every one of them broke in a different way:
#
#   - the heredoc is UNQUOTED, so bash expands `$` and backticks, and a regex
#     written `\s` arrives as `s`;
#   - a mutation written as `s.replace("literal", ...)` silently stops matching
#     the moment `cargo fmt` reflows the line, and then the "did it apply"
#     guard fires -- or worse, matches a PREFIX and splices half an expression
#     into the file, giving a brace error instead of a clean failure;
#   - a mutation that does not compile reports as "did not compile", which reads
#     as a no-op rather than as a broken script.
#
# So: one file per mutation, each a plain python script taking the target path,
# each ending in an `assert` that says which text it could not find. A failure
# names itself.
set -u
cd /home/alvaro/code-local/rust/commons || exit 1
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target/commons}"
export DATABASE_URL="${DATABASE_URL:-postgres://postgres:smoke_pw@127.0.0.1/postgres}"

SRC=crates/commons-server/src/media.rs
MUT=scripts/mutations
BACKUP=$(mktemp)
cp "$SRC" "$BACKUP"

# The test result is read FIRST. cargo prints `error: test failed, to rerun
# pass` at the end of every failed run, so a compile-error check placed first
# reports every killed mutant as "did not compile" -- which is what
# mutate-range.sh's first version did.
verdict() {
  local out
  out=$(cargo test -p commons-server --test media_route 2>&1)
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

echo "baseline (all pass = nothing to kill yet): $(verdict)"
echo

# --- the security boundary -------------------------------------------------
run_one caller_cannot_see.py             "the route asks as a caller who cannot see the object"
run_one denied_is_403.py                 "a denied object answers 403"

# --- the body must match the header ---------------------------------------
run_one body_is_whole_file.py            "the 206 body is the whole file, not the range"
run_one content_range_whole.py           "Content-Range claims the whole file"
run_one partial_announces_file_length.py "a 206 announces the file's length"

# --- the route's other claims ---------------------------------------------
run_one accept_ranges_dropped.py         "Accept-Ranges dropped from the 200"
run_one missing_file_is_500.py           "a missing file is a 500 rather than a 404"
run_one multipart_truncated.py           "multipart answers a 206 with only the first range"
run_one resolved_against_recorded.py     "the range is resolved against the index's recorded size"

cp "$BACKUP" "$SRC"
echo
echo "restored: $(verdict)"
rm -f "$BACKUP"
