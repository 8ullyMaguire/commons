"""Mutation-test T-P5-005's gesture rules.

A test suite that cannot fail is not evidence. Each mutation here removes or
inverts one decision from `gestures.ts` and asserts the unit tests notice.

The count that matters is not the mutations but the survivors: a survivor is a
rule nothing tests, and the whole point of extracting these four pure functions
was to make that list computable.

  run:  python3 scripts/mutate-gestures.py
"""

import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

UI = pathlib.Path(__file__).resolve().parent.parent / "ui"
SRC = UI / "src/lib/api/gestures.ts"
TEST = UI / "tests/gestures.test.ts"

# (name, pattern, replacement) -- each a one-line behavioural change.
MUTATIONS = [
    (
        "#7147 slop off by one: `>=` accepts the slop distance as a flick",
        r"if \(travelled > FLICK_SLOP\)",
        "if (travelled >= FLICK_SLOP)",
    ),
    (
        "#7147 slop disabled: any travel navigates",
        r"if \(travelled > FLICK_SLOP\)",
        "if (false)",
    ),
    (
        "#7147 duration ignored: a long press with no travel is a flick",
        r"if \(elapsed >= FLICK_MS\) \{\n(\s+)// A long press",
        r"if (false) {\n\1// A long press",
    ),
    (
        "#7147 distance measured from the moves, not the press (the original bug)",
        r"const dx = end\.x - start\.x;",
        "const dx = 0;",
    ),
    (
        "#7148 a wheel navigates on a pannable image",
        r"  if \(pannable\) \{\n    return 'pan';\n  \}",
        "  if (false) {\n    return 'pan';\n  }",
    ),
    (
        "#7148/#7149 the delta threshold disabled: every wheel navigates",
        r"Math\.abs\(deltaY\) >= WHEEL_NAVIGATE_DELTA \? 'navigate' : 'pan'",
        "'navigate'",
    ),
    (
        "#7148 the delta threshold is half what it claims",
        r"Math\.abs\(deltaY\) >= WHEEL_NAVIGATE_DELTA \?",
        "Math.abs(deltaY) * 2 >= WHEEL_NAVIGATE_DELTA ?",
    ),
    (
        "the dragging check is removed: a wheel mid-pan navigates again",
        r"  if \(dragging\) \{\n    return 'ignore';\n  \}",
        "  if (false) {\n    return 'ignore';\n  }",
    ),
    (
        "#7148/#7149 the sign of the delta decides the direction",
        r"Math\.abs\(deltaY\) >= WHEEL_NAVIGATE_DELTA \? 'navigate' : 'pan'",
        "deltaY >= WHEEL_NAVIGATE_DELTA ? 'navigate' : 'pan'",
    ),
    (
        "#7154 a dirty modal is closed by back, losing the edit",
        r"return dirty \? 're-push' : 'close';",
        "return 'close';",
    ),
    (
        "#7154 a clean modal re-pushes forever, so back never works",
        r"return dirty \? 're-push' : 'close';",
        "return 're-push';",
    ),
]


def run_unit_tests() -> tuple[bool, str]:
    r = subprocess.run(
        ["node", "./tests/run-tests.mjs"],
        cwd=UI,
        capture_output=True,
        text=True,
        timeout=300,
    )
    return r.returncode == 0, (r.stdout + r.stderr)


def main() -> int:
    source = SRC.read_text()
    test = TEST.read_text()

    print(f"mutations: {len(MUTATIONS)}")

    # The suite must pass on the unmutated source, or a "killed" verdict below
    # means nothing.
    ok, out = run_unit_tests()
    if not ok:
        print("FAIL: the suite does not pass on clean source")
        print(out[-2000:])
        return 1
    m = re.search(r"^. tests (\d+)", out, re.M)
    print(f"clean: {m.group(1) if m else '?'} tests pass")

    killed: list[str] = []
    survived: list[tuple[str, str]] = []
    exempt: list[str] = []

    backup_src = source
    backup_test = test

    for name, pattern, replacement in MUTATIONS:
        mutated, n = re.subn(pattern, replacement, backup_src)
        if n == 0:
            exempt.append(f"{name} (pattern did not match -- EXEMPT, re-check)")
            print(f"  EXEMPT  {name}")
            continue
        SRC.write_text(mutated)
        try:
            ok, out = run_unit_tests()
        finally:
            SRC.write_text(backup_src)
            TEST.write_text(backup_test)
        if ok:
            survived.append((name, out))
            print(f"  SURVIVED {name}")
        else:
            killed.append(name)
            print(f"  killed   {name}")

    print()
    print(f"killed {len(killed)}/{len(MUTATIONS)}")
    if survived:
        print("SURVIVORS (these rules are untested):")
        for name, out in survived:
            print(f"  - {name}")
            tail = [ln for ln in out.splitlines() if ln.startswith("# fail")]
            if tail:
                print(f"      (suite passed anyway; fail line: {tail[0]})")
    if exempt:
        print("exempt:")
        for e in exempt:
            print(f"  - {e}")
    return 1 if survived else 0


if __name__ == "__main__":
    sys.exit(main())
