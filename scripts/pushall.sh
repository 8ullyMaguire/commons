#!/usr/bin/env bash
# Push to both remotes. Refuses to report success if either push fails.
#
#   git pushall            # current branch
#   git pushall --tags     # branch and every milestone tag
#
# An earlier version was a `git config alias.pushall` shell function using `-q`.
# It exited 0 having pushed NOTHING: `-q` swallowed the output and the trailing
# `&&` made the function's status the second push's, so a total no-op looked
# like a clean push. Quiet plus a chain is how a silent failure gets a green
# exit code. Hence: no -q, and check both.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
for r in origin forgejo; do
  echo "==> $r"
  git push "$r" "$@"
done
echo "==> both remotes up to date"
