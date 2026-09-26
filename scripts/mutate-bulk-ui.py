#!/usr/bin/env python3
"""Mutation-test the client's bulk-result model.

`classify` and the message functions are the part of T-P5-006 item 3 a user
actually reads, and the part where a wrong answer is still a plausible sentence:
"Tagged 2 objects" and "Tagged 2 object" both look like working software.

So the mutants here concentrate on the decisions rather than the arithmetic --
the `blocked`/`empty` split, the `XOR` between applied and hidden, the
singular/plural boundary, and the two shapes of target.

Run from `ui/`:  python3 ../scripts/mutate-bulk-ui.py
"""
from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

UI = Path(__file__).resolve().parent.parent / "ui"
TARGET = UI / "src/lib/api/bulk.ts"


@dataclass(frozen=True)
class Mutant:
    name: str
    old: str
    new: str
    why: str


MUTANTS: list[Mutant] = [
    Mutant(
        name="a write with hidden rows reports as done",
        old="state = skipped > 0 ? 'partial' : 'done';",
        new="state = 'done';",
        why="The user is told their whole selection was applied.",
    ),
    Mutant(
        name="blocked becomes empty",
        old="state = 'blocked';",
        new="state = 'empty';",
        why="A permissions problem is reported as an empty library.",
    ),
    Mutant(
        name="any hidden row blocks the whole write",
        old="state = skipped > 0 ? 'partial' : 'done';",
        new="state = skipped > 0 ? 'blocked' : 'done';",
        why="One refused row reports the entire write as refused -- the "
            "over-correction, and the reason 'partial' exists.",
    ),
    Mutant(
        name="the plural branch is dropped",
        old="return res.applied === 1 ? 'Tagged 1 object.' : `Tagged ${res.applied} objects.`;",
        new="return `Tagged ${res.applied} objects.`;",
        why="'Tagged 1 objects.' is the kind of thing that ships.",
    ),
    Mutant(
        name="canApply ignores the blocked state",
        old="return res.state === 'done' || res.state === 'partial';",
        new="return res.state !== 'empty';",
        why="A refused write stays re-pressable.",
    ),
    Mutant(
        name="canApply ignores the empty state",
        old="return res.state === 'done' || res.state === 'partial';",
        new="return res.state !== 'blocked';",
        why="A write with nothing to do stays pressable.",
    ),
    Mutant(
        name="a select-all target degrades to ids",
        old="""  if (sel.query) {
    return { kind: 'QUERY', query: filterJson, excluded };
  }
  return { kind: 'IDS', ids: [...(sel.picked ?? [])], excluded };""",
        new="""  return { kind: 'IDS', ids: [...(sel.picked ?? [])], excluded };""",
        why="'Tag everything matching' becomes 'tag the 40 I loaded'.",
    ),
    Mutant(
        name="exclusions are dropped from the target",
        old="const excluded = [...(sel.excluded ?? [])];",
        new="const excluded: string[] = [];",
        why="A deselected row is silently written after all.",
    ),
    Mutant(
        name="an empty selection becomes everything",
        old="return { kind: 'IDS', ids: [...(sel.picked ?? [])], excluded };",
        new="return { kind: 'QUERY', query: filterJson, excluded };",
        why="Nothing selected is the most dangerous shape in the feature.",
    ),
]


def run(cmd: list[str]) -> tuple[int, str]:
    p = subprocess.run(cmd, cwd=UI, capture_output=True, text=True, timeout=900)
    return p.returncode, p.stdout + p.stderr


def main() -> int:
    src = TARGET.read_text()
    code, out = run(["node", "./tests/run-tests.mjs"])
    if code != 0:
        print(f"BASELINE FAILED -- not measuring mutants against a red suite.\n{out[-2000:]}")
        return 1
    print("baseline green\n")

    survived: list[Mutant] = []
    for m in MUTANTS:
        n = src.count(m.old)
        if n != 1:
            print(f"SKIP  {m.name}\n      anchor matched {n}x (must be 1)")
            survived.append(m)
            continue
        TARGET.write_text(src.replace(m.old, m.new, 1))
        code, out = run(["node", "./tests/run-tests.mjs"])
        TARGET.write_text(src)
        killed = code != 0
        if not killed:
            survived.append(m)
        names = re.findall(r"not ok \d+ - (\S+)", out) or re.findall(r"✖ (\S+)", out)
        print(f"{'KILL ' if killed else 'LIVE '} {m.name}")
        print(f"      {m.why}")
        if names:
            print(f"      caught by: {', '.join(sorted(set(names))[:3])}")

    print(f"\n{len(MUTANTS) - len(survived)}/{len(MUTANTS)} killed")
    for m in survived:
        print(f"  survived: {m.name}")
    return 1 if survived else 0


if __name__ == "__main__":
    raise SystemExit(main())
