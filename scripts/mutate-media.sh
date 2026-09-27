#!/usr/bin/env bash
# Mutation-check every load-bearing line in commons-store/src/media.rs.
#
# One mutation at a time, a full `cargo test` in between, and the source
# restored from a copy on disk rather than from a string held in the writer's
# memory. The first version of this kept `orig` in a Python variable and
# rewrote the file from it while cargo was still reading it; the result was a
# green run against a half-restored file, which is worse than no mutation check
# at all, because it reports "not killed" as "passed".
set -u
cd /home/alvaro/code-local/rust/commons || exit 1
export CARGO_TARGET_DIR=~/.cargo-target/commons
# The password comes from the caller's environment, never from this file: a
# script in a repo that carries a credential is a leak the day the repo is
# cloned, and the one that got the job done (embedding it, so the script was
# runnable without setup) is exactly the version that must not be committed.
: "${COMMONS_PGPW:?set COMMONS_PGPW, or export DATABASE_URL outright}"
export DATABASE_URL="postgres://postgres:${COMMONS_PGPW}@127.0.0.1/postgres"

TARGET=crates/commons-store/src/media.rs
BACKUP=$(mktemp)
cp "$TARGET" "$BACKUP"

run_tests() {
  cargo test -p commons-store --test media_db 2>&1 | grep -E '^test result' | head -1
}

# apply <label> <python-body>
apply() {
  local label="$1"
  cp "$BACKUP" "$TARGET"
  if ! python3 - "$TARGET" <<PY
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
  local r
  r=$(run_tests)
  case "$r" in
    *"0 failed"*) echo "$label: *** SURVIVED ***" ;;
    *"test result"*) echo "$label: killed" ;;
    *) echo "$label: no result (compile error?) -- $r" ;;
  esac
}

echo "baseline: $(run_tests)"
echo

apply "no consent clause (the security line)" \
"s = s.replace('WHERE {consent} AND f.object_id', 'WHERE f.object_id')"

apply "no consent_record join at all" \
"s = s.replace('INNER JOIN consent_record c ON c.object_id = o.id', '')"

apply "believe the engine's order (ORDER BY path)" \
"s = s.replace(chr(39) + 'present' + chr(39) + chr(34), chr(39) + 'present' + chr(39) + ' ORDER BY f.path' + chr(34))
s = s.replace('let chosen = raw.into_iter().min_by(|a, b| a.0.cmp(&b.0));', 'let chosen = raw.into_iter().next();')"

apply "no present-state filter" \
"s = s.replace(' AND f.state = ' + chr(39) + 'present' + chr(39), '')"

apply "no negative-size clamp" \
"s = s.replace('size_bytes: size.max(0) as u64,', 'size_bytes: size as u64,')"

# KNOWN SURVIVOR -- documented, not fixed. `consent_clause` emits `c.tier IN (...)`,
# a LEFT JOIN leaves c.tier NULL, and `NULL IN (...)` is not TRUE, so the row is
# filtered out either way. The INNER join is there so the query says what it
# means; a test claiming to pin it would claim coverage it does not have.
apply "LEFT JOIN consent (documented survivor)" \
"s = s.replace('INNER JOIN consent_record c', 'LEFT JOIN consent_record c')"

cp "$BACKUP" "$TARGET"
echo
echo "restored: $(run_tests)"
rm -f "$BACKUP"
