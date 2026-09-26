"""Mutation-test T-P5-006's selection rules.

Same contract as `mutate-gestures.py`: each mutation removes or inverts one
decision, and the suite must notice. A survivor is a rule nothing tests, which
in a selection model is a rule that can bulk-edit the wrong rows.

  run:  python3 scripts/mutate-selection.py
"""

import pathlib
import re
import subprocess
import sys

UI = pathlib.Path(__file__).resolve().parent.parent / "ui"
SRC = UI / "src/lib/api/selection.ts"
TEST = UI / "tests/selection.test.ts"

MUTATIONS = [
    (
        "exclusion is ignored, so a deselected row comes back",
        r"  if \(sel\.excluded\?\.has\(id\)\) return false;",
        "  // exemption",
    ),
    (
        "the query branch falls through to the picked check, so a select-all "
        "includes ids it never matched",
        r"  if \(sel\.query\) \{\n    if \(matchesQuery\(id\)\) return true;\n    return false;\n  \}",
        "  if (sel.query && matchesQuery(id)) return true;",
    ),
    (
        "the loaded count is reported as a total for a select-all",
        r"    if \(serverCount === undefined\) \{\n(?:.*\n)*?      return 'unknown';\n    \}",
        "    if (serverCount === undefined) {\n      return loadedIds.length;\n    }",
    ),
    (
        "exclusions are not subtracted from the select-all count",
        r"    return Math\.max\(0, serverCount - excluded\);",
        "    return serverCount;",
    ),
    (
        "the count goes negative when everything is excluded",
        r"    return Math\.max\(0, serverCount - excluded\);",
        "    return serverCount - excluded;",
    ),
    (
        "a stale server count is used after the select-all is gone",
        r"  if \(isSelectAll\(sel\)\) \{",
        "  if (true) {",
    ),
    (
        "setSelected(false) leaves a row in picked, so a range cannot clear it",
        r"  const picked = new Set\(sel\.picked \?\? \[\]\);\n  for \(const id of target\) \{\n    if \(selected\) picked\.add\(id\);\n    else picked\.delete\(id\);\n  \}",
        "  const picked = new Set(sel.picked ?? []);\n  for (const id of target) {\n    picked.add(id);\n  }",
    ),
    (
        "setSelected writes to picked under a select-all, so a deselect becomes a pick",
        r"  if \(sel\.query\) \{\n    const excluded = new Set\(sel\.excluded \?\? \[\]\);",
        "  if (false) {\n    const excluded = new Set(sel.excluded ?? []);",
    ),
    (
        "setSelected is a no-op, so a range silently does nothing",
        r"export function setSelected\(sel: Selection, ids: Iterable<ObjectId>, selected: boolean\): Selection \{",
        "export function setSelected(sel: Selection, ids: Iterable<ObjectId>, selected: boolean): Selection {\n  if (true) return sel;",
    ),
    (
        "a select-all survives a change of result",
        r"  if \(sel\.query\) \{\n    return \{ selection: emptySelection\(\), dropped: 0 \};\n  \}",
        "  if (false) {\n    return { selection: emptySelection(), dropped: 0 };\n  }",
    ),
    (
        "the dropped count is always zero, so the warning is silent",
        r"    if \(presentIds\.has\(id\)\) kept\.add\(id\);\n    else dropped \+= 1;",
        "    if (presentIds.has(id)) kept.add(id);",
    ),
    (
        "a prune that drops nothing still rebuilds the set",
        r"  if \(dropped === 0\) return \{ selection: sel, dropped: 0 \};",
        "  // exemption",
    ),
    (
        "a failed save with nothing pending is not unsaved",
        r"  return hasFailedSave \|\| pendingEdits > 0;",
        "  return pendingEdits > 0;",
    ),
    (
        "a saved edit is still unsaved",
        r"  return hasFailedSave \|\| pendingEdits > 0;",
        "  return hasFailedSave || pendingEdits >= 0;",
    ),
    (
        "toggling under a select-all writes to picked, not excluded",
        r"  if \(sel\.query\) \{\n    if \(excluded\.has\(id\)\) excluded\.delete\(id\);\n    else excluded\.add\(id\);\n    return \{ \.\.\.sel, excluded \};\n  \}",
        "  if (sel.query) {\n    if (picked.has(id)) picked.delete(id);\n    else picked.add(id);\n    return { ...sel, picked };\n  }",
    ),
    (
        "setPicked keeps a stale query, so a replaced selection is still a "
        "select-all",
        r"export function setPicked\(ids: Iterable<ObjectId>\): Selection \{\n  return \{ picked: new Set\(ids\) \};\n\}",
        "export function setPicked(ids: Iterable<ObjectId>): Selection {\n"
        "  return { query: { filter: 'stale' }, picked: new Set(ids) };\n}",
    ),
    (
        "setPicked keeps a stale exclusion list, so the rows it once turned "
        "off are still off",
        r"export function setPicked\(ids: Iterable<ObjectId>\): Selection \{\n  return \{ picked: new Set\(ids\) \};\n\}",
        "export function setPicked(ids: Iterable<ObjectId>): Selection {\n"
        "  return { picked: new Set(ids), excluded: new Set(['a']) };\n}",
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
    return r.returncode == 0, r.stdout + r.stderr


def main() -> int:
    backup = SRC.read_text()
    test = TEST.read_text()

    print(f"mutations: {len(MUTATIONS)}")

    ok, out = run_unit_tests()
    if not ok:
        print("FAIL: the suite does not pass on clean source")
        print(out[-2000:])
        return 1
    m = re.search(r"^. tests (\d+)", out, re.M)
    print(f"clean: {m.group(1) if m else '?'} tests pass")

    killed, survived, exempt = [], [], []

    for name, pattern, replacement in MUTATIONS:
        mutated, n = re.subn(pattern, replacement, backup)
        if n == 0:
            exempt.append(name)
            print(f"  EXEMPT   {name} (pattern did not match)")
            continue
        SRC.write_text(mutated)
        try:
            ok, _ = run_unit_tests()
        finally:
            SRC.write_text(backup)
            TEST.write_text(test)
        (killed if not ok else survived).append(name)
        print(f"  {'killed  ' if not ok else 'SURVIVED'} {name}")

    print()
    print(f"killed {len(killed)}/{len(MUTATIONS)}")
    for name in survived:
        print(f"  SURVIVED: {name}")
    for name in exempt:
        print(f"  EXEMPT:   {name}")
    return 1 if survived or exempt else 0


if __name__ == "__main__":
    sys.exit(main())
