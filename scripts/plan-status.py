#!/usr/bin/env python3
"""Report the real state of every ticket in the implementation plan.

The plan grew two conventions for marking a ticket finished -- "DONE" in the
heading, and a `**Done**` line under it -- and a third style, a paragraph that
reads like a completion note with neither. Reading the plan by eye therefore
answers "how far along are we" with a different number every time, and the
answer is wrong in whichever direction the reader happens to skim.

A ticket counts as closed when it carries an explicit marker:

  * `**Done**` at the start of a line, or
  * `DONE` in the heading itself.

That is a claim about the *document*, not about the tree. So this script
cross-checks every claim against the file each ticket says it lives in, and
reports the two ways a plan goes wrong:

  * **claims done, no file** -- the plan says finished and the file is absent.
    This is the expensive failure: it reads as progress and builds as nothing.
  * **unmarked, file present** -- the work is in the tree and the plan does not
    say so. Cheap to fix, and it hides real progress.

Run it with no arguments for a summary, or `--verbose` for per-ticket detail.
Exit status is 1 when any claim is false, so it can gate a commit.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PLAN = REPO / "docs" / "plans" / "implementation-plan.md"

HEADING = re.compile(r"^### (T-P\d+-\d+) — (.*)$", re.M)
DONE_LINE = re.compile(r"^\*\*Done\*\*", re.M)
FILES_LINE = re.compile(r"^\*\*Files:\*\*(.*)$", re.M)
BACKTICKED = re.compile(r"`([^`]+)`")


def tickets(text: str) -> list[dict]:
    """Every ticket, with its body, in document order."""
    heads = list(HEADING.finditer(text))
    out = []
    for i, m in enumerate(heads):
        end = heads[i + 1].start() if i + 1 < len(heads) else len(text)
        body = text[m.end() : end]
        out.append(
            {
                "id": m.group(1),
                "title": m.group(2).strip(),
                "body": body,
                "claimed": bool(DONE_LINE.search(body)) or "DONE" in m.group(2),
            }
        )
    return out


def paths_for(body: str) -> list[str]:
    """The repo-relative paths a ticket names, de-duplicated, order preserved."""
    line = FILES_LINE.search(body)
    if not line:
        return []
    seen, out = set(), []
    for p in BACKTICKED.findall(line.group(1)):
        if "/" not in p or p.startswith("§") or p in seen:
            continue
        seen.add(p)
        out.append(p)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--verbose", "-v", action="store_true", help="per-ticket detail")
    args = ap.parse_args()

    if not PLAN.exists():
        print(f"no plan at {PLAN}", file=sys.stderr)
        return 2

    rows = []
    for t in tickets(PLAN.read_text()):
        paths = paths_for(t["body"])
        # A path with a glob is satisfied if any match exists.
        missing = []
        for p in paths:
            if any(ch in p for ch in "*?"):
                if not list(REPO.glob(p.lstrip("./"))):
                    missing.append(p)
            elif not (REPO / p.lstrip("./")).exists():
                missing.append(p)
        present = [p for p in paths if p not in missing]
        rows.append({**t, "paths": paths, "missing": missing, "present": present})

    claimed = [r for r in rows if r["claimed"]]
    false_claims = [r for r in claimed if r["missing"]]
    unmarked = [r for r in rows if not r["claimed"] and r["present"]]
    # Unmarked *and* nothing on disk: genuinely not started. Unmarked and
    # nothing named: a ticket that points at a directory, or at prose.
    unstarted = [
        r for r in rows if not r["claimed"] and not r["present"] and r["paths"]
    ]

    print(f"plan: {PLAN.relative_to(REPO)}")
    print(f"tickets: {len(rows)}")
    print(f"claimed done: {len(claimed)}")
    print(f"  ...with every named file present: {len(claimed) - len(false_claims)}")
    print(f"  ...claiming a file that is absent: {len(false_claims)}")
    print(f"not claimed, but the file is there: {len(unmarked)}")
    print(f"not claimed, nothing on disk: {len(unstarted)}")

    if false_claims:
        print("\nCLAIMED DONE, FILE ABSENT -- the expensive failure:")
        for r in false_claims:
            print(f"  {r['id']}  {r['title'][:52]}")
            for p in r["missing"]:
                print(f"      missing: {p}")
    if unmarked:
        print("\nBUILT BUT UNMARKED -- real progress the plan does not claim:")
        for r in unmarked:
            print(f"  {r['id']}  {r['title'][:52]}")
    if args.verbose:
        print("\nevery ticket:")
        for r in rows:
            flag = "done" if r["claimed"] else "open"
            if r["claimed"] and r["missing"]:
                flag = "FALSE"
            elif not r["claimed"] and r["present"]:
                flag = "unmarked"
            print(f"  {flag:9} {r['id']}  {r['title'][:56]}")

    return 1 if false_claims else 0


if __name__ == "__main__":
    sys.exit(main())
