#!/usr/bin/env bash
# Push the branch AND every tag to both remotes, and verify what landed.
#
#   git pushall            # the branch
#   git pushall --tags     # the branch and every milestone tag
#
# ## Why this is a script and not a git alias
#
# An earlier version was a `git config alias.pushall` shell function using `-q`.
# It exited 0 having pushed NOTHING: `-q` swallowed the output and the trailing
# `&&` made the function's status the second push's, so a total no-op looked
# like a clean push. Quiet plus a chain is how a silent failure gets a green
# exit code. Hence: no -q, and check both.
#
# ## Why the refspec is spelled out, and why that is not redundant
#
# `git push "$r" --tags` pushes **tags only**. Not the branch. It is not a
# shorthand for "branch and tags" -- `--tags` is a refspec, and naming it
# *replaces* the default `refs/heads/*` refspec rather than adding to it.
#
# That is not a theoretical mistake. It is what this script did for five
# commits: every milestone tag reached the remote, so the commit objects were
# fetchable by SHA and by tag, and `main` stayed five commits behind the whole
# time. Everything looked fine:
#
#   - `git ls-remote --tags` listed every tag, so a tag check passed;
#   - the commits were reachable, so a plain `git fetch` brought them in;
#   - the local commit log looked right, because it was.
#
# The remote-sync gate in `scripts/verify.sh` is what caught it, by comparing
# `refs/heads/main` rather than the tags. Which is the argument for that gate
# existing -- and the reason this script now verifies the branch head after
# pushing rather than trusting the exit code.
#
# So: the branch refspec is explicit, `--tags` is a *separate* invocation
# rather than a flag on the same one, and the result is checked against
# `ls-remote` -- the same evidence the gate uses, so the script and the gate
# cannot disagree about what "pushed" means.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# `HEAD` rather than the current branch name: if HEAD is detached, a push of
# "the current branch" silently means something else, and a milestone is not
# the moment to discover that.
head_ref=$(git symbolic-ref --quiet HEAD || echo HEAD)
branch=${head_ref##*/}

status=0
for r in origin forgejo; do
  echo "==> $r"
  if ! git push "$r" "$head_ref:refs/heads/$branch"; then
    echo "   $r: branch push FAILED"
    status=1
  fi
  if [ "${1:-}" = "--tags" ]; then
    # Its own invocation. Adding --tags to the command above would REPLACE the
    # branch refspec -- that is the bug this file exists to prevent.
    if ! git push "$r" --tags; then
      echo "   $r: tag push FAILED"
      status=1
    fi
  fi

  # Verify rather than trust. `ls-remote` asks the remote, so this is the same
  # evidence `verify.sh` uses, and a push that reported success without moving
  # the ref cannot pass here.
  #
  # `|| true` on both, deliberately. Under `set -e` an unreachable remote kills
  # the script at the FIRST failing command, so a Forgejo outage would abort
  # before origin's own result was reported -- the one thing this script exists
  # to tell you is which remote is behind. An empty `remote_head` is the
  # unreachable case and it compares unequal to anything, so it is already a
  # failure without needing a separate branch.
  remote_head=$(git ls-remote "$r" "refs/heads/$branch" 2>/dev/null | cut -f1 || true)
  local_head=$(git rev-parse HEAD)
  if [ -z "$remote_head" ]; then
    echo "   $r: UNREACHABLE -- cannot say whether it is up to date"
    status=1
  elif [ "$remote_head" != "$local_head" ]; then
    echo "   $r: $branch is at ${remote_head:0:9}, local is ${local_head:0:9} -- NOT PUSHED"
    status=1
  else
    echo "   $r: $branch at ${local_head:0:9}"
    if [ "${1:-}" = "--tags" ]; then
      # Distinct tag NAMES, not refs: an annotated tag makes ls-remote emit two
      # lines (the tag and its peeled "^{}"), so counting lines reports fewer
      # tags than exist and reads as missing ones. Same reason as the gate's.
      remote_tags=$(git ls-remote --tags "$r" 2>/dev/null | cut -f2 | sed 's@refs/tags/@@' | sed 's@\^{}@@' | sort -u | wc -l | tr -d ' ' || true)
      local_tags=$(git tag | wc -l | tr -d ' ')
      if [ "$remote_tags" != "$local_tags" ]; then
        echo "   $r: $remote_tags of $local_tags tags -- NOT ALL PUSHED"
        status=1
      else
        echo "   $r: all $local_tags tags"
      fi
    fi
  fi
done

if [ "$status" -ne 0 ]; then
  echo "==> one or both remotes did not receive everything. Not reporting success."
  exit 1
fi
echo "==> both remotes verified at $(git rev-parse --short HEAD)"
