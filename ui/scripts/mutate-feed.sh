#!/usr/bin/env bash
# Mutation-check the feed's thresholds in the component, where they are
# actually used.
#
# The unit tests in ui/tests/feed.test.ts cover the pure functions. This checks
# the other half of the claim: that the COMPONENT consults those functions. A
# component with a hardcoded threshold and a perfect test of `classifyCommit`
# passes every unit test and ignores the threshold entirely -- which is why the
# drag test in the e2e spec drags 10% and 60% rather than 29% and 31%.
set -u
cd /home/alvaro/code-local/rust/commons/ui || exit 1

COMP=src/lib/components/VerticalFeed.svelte
BACKUP=$(mktemp)
cp "$COMP" "$BACKUP"

# The verdict, and getting it right took two tries.
#
# Playwright prints "  1 failed" on its own line and "  5 passed (7.7s)" on
# another, so `tail -1` sees only the pass line and a killed mutant reads as a
# survivor. The first version grepped for "0 failed", which Playwright never
# prints at all, so it reported the inverse of the truth for every mutant.
#
# The verdict is the COUNT LINE with "failed" on it, which Playwright only emits
# when something died, and which it omits entirely on a clean run.
build_and_run() {
  node node_modules/vite/bin/vite.js build >/dev/null 2>&1 || { echo "BUILD FAILED"; return 1; }
  local out
  out=$(CI=1 npx playwright test e2e/vertical-feed.spec.ts 2>&1)
  if printf '%s' "$out" | grep -qE '^  [0-9]+ failed'; then
    echo "killed"
  elif printf '%s' "$out" | grep -qE '^  [0-9]+ passed'; then
    echo "survived"
  else
    echo "no result"
  fi
}

apply() {
  local label="$1"
  cp "$BACKUP" "$COMP"
  if ! python3 - "$COMP" <<PY
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
  r=$(build_and_run)
  case "$r" in
    killed) echo "$label: killed" ;;
    survived) echo "$label: *** SURVIVED ***" ;;
    *) echo "$label: $r" ;;
  esac
}

echo "baseline: $(build_and_run)"
echo

# The threshold replaced by a constant that a 10% drag and a 60% drag BOTH
# cross. A 0.05 threshold still commits on both, so this only dies if the
# component is really asking classifyCommit.
apply "threshold 0.3 -> 0.05 (a 10% drag would commit)" \
"s = s.replace('const verdict = classifyCommit(dy, viewportH);',
              'const verdict = classifyCommit(dy, viewportH) === undefined ? { kind: \"spring-back\" } : (Math.abs(dy) > viewportH * 0.05 ? { kind: \"commit\", axis: dy < 0 ? \"up\" : \"down\" } : { kind: \"spring-back\" });')"

# The axis inverted. The unit test pins this on classifyCommit; this checks the
# component passes the verdict through rather than deciding again.
apply "axis inverted in the component" \
"s = s.replace('onfocuschange?.(nextFocus(focus, verdict.axis === \\'up\\' ? 1 : -1, items.length));',
              'onfocuschange?.(nextFocus(focus, verdict.axis === \\'up\\' ? -1 : 1, items.length));')"

# Focus ignored: every slide plays, and every slide is focused.
apply "isPlaying always true" \
"s = s.replace('data-playing={isPlaying(i, focus) ? \\'true\\' : \\'false\\'}', 'data-playing=\\'true\\'')"

# Preload distance ignored: everything preloads.
apply "everything preloads" \
"s = s.replace('preload={preloadAttr(i, focus, items.length)}', 'preload=\\'auto\\'')"

# The window removed from the ROUTE. A file copy, not `git checkout`: the route
# is untracked until this work is committed, and `git checkout` on an untracked
# path DELETES it -- which is how an earlier version of a sibling script lost
# the file it was supposed to be checking.
cp "$BACKUP" "$COMP"
ROUTE=src/routes/vertical/+page.svelte
cp "$ROUTE" /tmp/route.bak
python3 - <<'PY'
p = 'src/routes/vertical/+page.svelte'
s = open(p).read()
s2 = s.replace('return rows.slice(start, start + WINDOW);', 'return rows;')
assert s2 != s, 'the route window was not found'
open(p, 'w').write(s2)
PY
r=$(build_and_run)
case "$r" in
  *"0 failed"*) echo "the route renders every row: *** SURVIVED ***" ;;
  *failed*) echo "the route renders every row: killed" ;;
  *) echo "the route renders every row: no result -- $r" ;;
esac
cp /tmp/route.bak "$ROUTE"
rm -f /tmp/route.bak

cp "$BACKUP" "$COMP"
echo
echo "restored: $(build_and_run)"
rm -f "$BACKUP"
