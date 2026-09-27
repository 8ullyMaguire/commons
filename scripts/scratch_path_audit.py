#!/usr/bin/env python3
"""No test scratch path may be keyed on the pid alone.

**Pids are reused.** Two runs of a test binary days apart get the same pid, so
a `temp_dir().join(format!("...-{tag}-{pid}"))` names the same directory twice
over a machine's lifetime. The second run's `remove_dir_all` then deletes a
stub script a sibling thread is still `exec`ing, and the kernel answers
`ETXTBSY -- "Text file busy"`. It surfaces as a spawn failure inside whichever
test happened to be running, which is why this cost an hour to read the first
time: the failure is in a test named for a deadlock that never happened, on a
file the test did not touch.

The rule is that uniqueness must hold *across runs*, not merely within one, so a
pid is only acceptable alongside something that changes between runs: a
timestamp, a random value, or a uuid.

Run: `python3 scripts/scratch_path_audit.py`
"""

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# What makes a path unique across runs rather than only within one.
ACROSS_RUNS = re.compile(r"now\(|SystemTime|Instant|random|rand::|Uuid|uuid|nanos|thread_rng|getrandom")

# A pid used in a path at all. Not banned -- allowed only next to a timestamp.
PID = re.compile(r"process::id\(\)")

# **A join is the other way to be unique, and the audit has to know it.**
# `runtime.path().join("x-<pid>")` is safe when `runtime.path()` is already
# unique per call, because the leaf name can only collide inside a directory
# this run owns. What is NOT safe is `temp_dir().join("x-<pid>")`, where the
# parent is the one directory every run on the machine shares.
#
# So: a pid is fine when the path is joined onto a receiver that is not
# `temp_dir()`, and a pid is a failure when the parent IS `temp_dir()` and
# nothing else varies. The distinction is not cosmetic -- inverting it lets a
# real collision through, and a test asserting on a lock file the supervisor
# never writes fails in a way that looks like a product bug.
TEMP_DIR_JOIN = re.compile(r"temp_dir\(\)\s*\.\s*join")

failures: list[str] = []
checked = 0

for path in sorted((REPO / "crates").rglob("tests/*.rs")):
    text = path.read_text()
    if not PID.search(text):
        continue
    # Every *line window* that mentions a pid must also mention something that
    # varies per run. A helper 20 lines below the pid is not a sibling of it.
    lines = text.split("\n")
    for i, line in enumerate(lines):
        if not PID.search(line):
            continue
        # The `format!` that uses the pid usually wraps, so the evidence that a
        # path is unique across runs can sit on either side of the pid line.
        # The window is symmetric and wide enough for a multi-line format!.
        window = "\n".join(lines[max(0, i - 14) : i + 15])
        checked += 1
        rel = path.relative_to(REPO)
        if ACROSS_RUNS.search(window):
            continue
        # A join onto something other than `temp_dir()` means the parent is
        # already unique, so the leaf name cannot collide across runs.
        if re.search(r"\)\s*\.\s*join", window) and not TEMP_DIR_JOIN.search(window):
            continue
        failures.append(
            f"{rel}:{i + 1}: a pid keys a scratch path with nothing that "
            f"varies between runs\n    {line.strip()}"
        )

if failures:
    print("FAIL: a scratch path is unique only within one run")
    for f in failures:
        print(f"  {f}")
    print(
        "\n  Add a timestamp or uuid beside the pid. A counter is not enough --\n"
        "  it is per-process, so it dies with the pid."
    )
    sys.exit(1)

print(f"ok: {checked} pid-keyed path(s), each with a per-run component too")
