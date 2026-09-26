#!/usr/bin/env python3
"""Mutation-test the unsaved guard's rules.

`guard.ts` is the part of T-P5-006 item 4 a reviewer reads, and the part where
a wrong answer is silent: `isDirty` returning false loses an edit, `summary`
returning the wrong count tells a user their work is safe when it is not.

The registry is a Map keyed by surface id, so each mutant is a single-line
source edit plus one command to prove something died. A mutant that survives is
a gap in the tests, and every gap this script has found so far has been in the
tests rather than the code -- an assertion on a boolean that any nearby change
would also satisfy.

Run: python3 scripts/mutate-guard-ui.py
"""
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
TARGET = REPO / "ui/src/lib/api/guard.ts"
UNIT = "tests/guard.test.ts"
ORIGINAL = TARGET.read_text()

# (name, the line to replace, the replacement, why it matters)
MUTANTS = [
    (
        "an empty registry produces a message",
        "  if (surfaces.length === 0) return null;",
        "  if (false) return null;",
        "With nothing registered, this builds \"You have .\" and the template "
        "renders it. The confirm then opens on every navigation with a message "
        "that says nothing, which is worse than silence: it looks broken rather "
        "than absent.",
    ),
    (
        "a clean surface counts as dirty",
        "    if (isDirtyOne(s)) return true;",
        "    if (!isDirtyOne(s)) return true;",
        "Inverted: a page with one saved edit prompts, and a page with one "
        "unsaved edit does not. Exactly inverted, so the tests that check both "
        "directions are the ones that kill this.",
    ),
    (
        "dirtyCount counts clean surfaces",
        "  for (const s of reg.values()) if (isDirtyOne(s)) n += 1;",
        "  for (const s of reg.values()) if (!isDirtyOne(s)) n += 1;",
        "The count in the prompt is the number of edits at risk. Inverted, it "
        "is the number that is safe.",
    ),
    (
        "the failed-save clause is dropped from the message",
        "  if (failed > 0) {",
        "  if (false) {",
        "A user whose save failed is told only that they have unsaved changes, "
        "and leaves believing their work is held somewhere. It is not.",
    ),
    (
        "the pending clause is dropped from the message",
        "  if (pending > 0) {",
        "  if (false) {",
        "With the failed clause present this still reads, so the message is "
        "plausible and wrong for the common case: unsaved work with nothing "
        "failed reports nothing at all.",
    ),
    (
        "singular and plural are swapped",
        "    parts.push(pending === 1 ? '1 unsaved change' : `${pending} unsaved changes`);",
        "    parts.push(pending === 1 ? '1 unsaved changes' : `${pending} unsaved change`);",
        "Cosmetic on its face, and the reason it is here: a number and its noun "
        "disagreeing is the kind of thing that ships because nobody asserted "
        "the agreement.",
    ),
    (
        "the surface label is dropped",
        "  if (named?.label) {",
        "  if (false) {",
        "The prompt no longer says which object. The user cannot tell one edit "
        "from three, which is the whole reason the scope is spelled out.",
    ),
    (
        "consume keeps the registry",
        "  return reg.size === 0 ? reg : emptyRegistry();",
        "  return reg;",
        "The user chose to leave and is asked again on the way out. This is the "
        "failure the spec calls its whole mechanism, and it is invisible in a "
        "unit test -- it needs the ordering, which the e2e covers.",
    ),
    (
        "an empty id list becomes no predicate",
        "  return new Map();",
        "  return reg;",
        "Only reachable through consume; makes the same failure as above by a "
        "different route, and both must die for the ordering to be trustworthy.",
    ),
    (
        "clear forgets an unknown id",
        "  if (!reg.has(id)) return reg;",
        "  if (!reg.has(id)) return new Map();",
        "Deregistering a surface that is not there rewrites the registry and "
        "notifies every reader, for no change in content.",
    ),
]


def run_unit() -> bool:
    r = subprocess.run(
        ["node", "./tests/run-tests.mjs", UNIT],
        cwd=REPO / "ui",
        capture_output=True,
        text=True,
        timeout=600,
    )
    return r.returncode == 0


def main() -> int:
    if "run-tests.mjs" not in (REPO / "ui/tests/run-tests.mjs").read_text():
        pass  # the harness may not support filtering; fall back to the full run

    if not run_unit():
        print("baseline RED -- the unit suite does not pass on unmodified source")
        print("fix that before reading anything below: a mutant that survives a")
        print("red baseline is not evidence of anything")
        return 1
    print("baseline green\n")

    killed = 0
    survivors = []
    for name, old, new, why in MUTANTS:
        if old not in ORIGINAL:
            print(f"SKIP  {name}\n      pattern not found -- update the mutant")
            survivors.append(name + " (pattern not found)")
            continue
        TARGET.write_text(ORIGINAL.replace(old, new, 1))
        if run_unit():
            print(f"LIVE  {name}\n      {why}")
            survivors.append(name)
        else:
            print(f"KILL  {name}\n      {why}")
            killed += 1

    TARGET.write_text(ORIGINAL)
    total = len(MUTANTS)
    print(f"\n{killed}/{total} killed")
    if survivors:
        print("  survived: " + ", ".join(survivors))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
