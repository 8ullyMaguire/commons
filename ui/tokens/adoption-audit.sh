#!/usr/bin/env bash
# How far is each front end from the token set? Item 5.3's exit test, as a script rather than a vibe.
#
# Counts literal 6-digit hex colours in the CSS/component sources a project actually owns, ignoring
# generated build output, vendored dependencies and the token package itself. The target is 0 in every
# "shared surface" (the chrome a reader sees on every page); project-specific artwork is allowed to have its own
# colours, which is why repos are counted separately instead of one global number.
#
# Usage:
#   ./adoption-audit.sh                 # table for every project it can find
#   ./adoption-audit.sh --check         # exit 1 if any project is above its recorded budget
#   ./adoption-audit.sh --write-budget  # record current counts as the budget (do this once, then only go down)
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${ESTATE_ROOT:-/home/alvaro/code-local}"
BUDGET="$HERE/adoption-budget.tsv"

# Trees the owner is actively working in. Their budgets are printed for information only: an agent must never
# "fix" a literal colour in a tree it does not own, so failing the check on them would demand the one action
# that is forbidden. Observed once already: ascend went 6 -> 7 by the owner's own hand.
OWNER_TREES=" ascend darkhold "

# Project -> the directories a reader's chrome lives in. Deliberately narrow: counting docs/ or generated
# build output would measure the wrong thing and never reach zero.
TARGETS=(
  "ascend|web/src"
  "darkhold|frontend/src"
  "streakforge|web/src"
  "tessera|web/src"
  "lorebook|ui/src"
  "whitebois|web/src"
  "gravity|frontend/src"
  "polaris|web/src"
  "stash-box|frontend/src"
)

# A file may opt out of the count, but only by declaring itself a brand palette on its first three lines:
#     /* estate-tokens:brand-file -- this file IS the brand layer */
# That keeps a project's identity colours in one auditable place instead of scattered through components, which
# is the failure this audit exists to find. Scattering is still counted.
count_hex() {
  local dir="$1"
  [[ -d "$dir" ]] || { echo 0; return; }
  local total=0 f n
  while IFS= read -r f; do
    head -8 "$f" | grep -q "estate-tokens:brand-file" && continue
    # <meta name="theme-color"> cannot take a CSS variable, so that one line is exempt -- a literal there is
    # forced by the platform, not by carelessness. Everything else has to be a token.
    n=$(grep -v 'theme-color' "$f" | grep -oE '#[0-9a-fA-F]{6}\b' | grep -viE '^#(f{3,6}|0{3,6})$' | wc -l)
    total=$((total + n))
  done < <(find "$dir" -type f \( -name '*.css' -o -name '*.svelte' -o -name '*.html' -o -name '*.js' -o -name '*.ts' -o -name '*.astro' \) 2>/dev/null)
  echo "$total"
}

rows=""
for entry in "${TARGETS[@]}"; do
  name="${entry%%|*}"; dirs="${entry#*|}"
  total=0; scanned=0
  # Projects live under rust/, go/ or the blackrep.app umbrella -- try each, do not assume one layout.
  for base in "$ROOT/rust/$name" "$ROOT/go/$name" "$ROOT/$name" "$ROOT/blackrep.app/$name"; do
    [[ -d "$base" ]] || continue
    for rel in $dirs; do
      for d in "$base/"$rel; do
        [[ -d "$d" ]] || continue
        total=$((total + $(count_hex "$d"))); scanned=$((scanned + 1))
      done
    done
  done
  [[ $scanned -eq 0 ]] && continue
  rows+="${name}"$'\t'"${total}"$'\n'
done

if [[ "${1:-}" == "--write-budget" ]]; then
  printf "%b" "$rows" | sort >"$BUDGET"
  echo "adoption-audit: wrote $(wc -l <"$BUDGET") budgets to $BUDGET"
  exit 0
fi

printf "%-14s %8s %8s %s\n" project literals budget status
fail=0
while IFS=$'\t' read -r name count; do
  [[ -z "$name" ]] && continue
  budget="—"
  if [[ -f "$BUDGET" ]]; then
    b=$(awk -F'\t' -v n="$name" '$1==n{print $2}' "$BUDGET")
    [[ -n "$b" ]] && budget="$b"
  fi
  owner=""
  [[ "$OWNER_TREES" == *" $name "* ]] && owner=" (owner tree)"
  status="untracked"
  if [[ "$budget" != "—" ]]; then
    if [[ "$count" -le "$budget" ]]; then status="ok"
    else
      status="OVER by $((count - budget))"
      [[ -z "$owner" ]] && fail=1
    fi
    [[ "$count" -eq 0 ]] && status="adopted"
  fi
  printf "%-14s %8s %8s %s%s\n" "$name" "$count" "$budget" "$status" "$owner"
done < <(printf "%b" "$rows" | sort)

if [[ "${1:-}" == "--check" ]]; then
  if [[ $fail -ne 0 ]]; then
    echo "adoption-audit: a project gained literal colours -- map them onto a --token instead" >&2
    exit 1
  fi
  echo "adoption-audit: no project above budget"
fi
