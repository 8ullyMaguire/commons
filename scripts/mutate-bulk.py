#!/usr/bin/env python3
"""Mutation-test the bulk write path.

The negative test -- an invisible object is not written -- is the one that
matters, and it is the one most likely to be satisfied by accident. So the
mutants below are concentrated on the consent clause and on the count, and the
script reports per-mutant rather than a single number: a bulk path whose only
green mutant is one nobody read is not a passing path.

Run:  python3 scripts/mutate-bulk.py [--keep]
"""
from __future__ import annotations

import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TARGET = REPO / "crates/commons-store/src/bulk.rs"
# Only the bulk suite: these mutants must not be able to be killed by an
# unrelated test, or the script would report credit for a test that never
# exercised the changed line.
TEST = "bulk"


@dataclass(frozen=True)
class Mutant:
    name: str
    old: str
    new: str
    why: str


MUTANTS: list[Mutant] = [
    Mutant(
        name="consent clause dropped from the count and the insert",
        old="WHERE {consent} AND ({user})",
        new="WHERE ({user})",
        why="The whole point of the module. An invisible object gets written.",
    ),
    Mutant(
        name="count_visible drops the consent clause",
        old="""        let sql = format!("SELECT COUNT(*) {from_where}");""",
        new="""        let user_pred: String = match target {
            Target::Ids(ids) => {
                if ids.is_empty() { "1 = 0".to_string() } else { "1 = 1".to_string() }
            }
            Target::Filter(_) => "1 = 1".to_string(),
        };
        let sql = format!("SELECT COUNT(*) FROM object o WHERE ({user_pred})");
        let _ = &params;""",
        why="Over-reports: the insert is still safe but the count lies.",
    ),
    Mutant(
        name="empty id list becomes no predicate",
        old='"1 = 0".to_string()',
        new='"1 = 1".to_string()',
        why="A user who selected nothing tags the whole library.",
    ),
    Mutant(
        name="reported count is the request size, not the server's",
        old="let applied = self.bulk_count_visible(target, caller).await?;",
        new="let applied = match target { Target::Ids(ids) => ids.len(), Target::Filter(_) => 0 };",
        why="The number a modal shows the user is the number the client sent.",
    ),
    Mutant(
        name="id target ignores the consent filter",
        old="""        Target::Ids(ids) => {
            if ids.is_empty() {""",
        new="""        Target::Ids(ids) => {
            if true {""",
        why="The shape everyone forgets to filter.",
    ),
    Mutant(
        name="provenance is dropped from a bulk write",
        old="SET confidence = excluded.confidence, source = excluded.source",
        new="SET confidence = excluded.confidence",
        why="A bulk tag stops saying where it came from.",
    ),
    Mutant(
        name="a missing tag is not caught before the batch",
        old="if !self.tag_exists(tag_id).await? {",
        new="if false {",
        why="Only the foreign key catches it, and only after the statement ran.",
    ),
    Mutant(
        name="idempotence lost: a duplicate row per application",
        old="ON CONFLICT (object_id, tag_id) DO UPDATE",
        new="ON CONFLICT (object_id, tag_id) DO NOTHING",
        why="Weaker than the fix but still no duplicates, so this is the floor.",
    ),
]


def run(cmd: list[str], cwd: Path) -> tuple[int, str]:
    p = subprocess.run(
        cmd, cwd=cwd, capture_output=True, text=True, timeout=1800,
        env={**__import__("os").environ,
             "DATABASE_URL": "postgres://postgres:smoke_pw@127.0.0.1/postgres",
             "CARGO_TARGET_DIR": str(Path.home() / ".cargo-target/commons")},
    )
    return p.returncode, p.stdout + p.stderr


def main() -> int:
    keep = "--keep" in sys.argv
    src = TARGET.read_text()
    # A green baseline first: a mutant "killed" by a suite that was already red
    # is a false credit, and the whole report would be noise.
    code, out = run(["cargo", "test", "-p", "commons-store", "--test", TEST], REPO)
    if code != 0:
        print(f"BASELINE FAILED -- not measuring mutants against a red suite.\n{out[-2000:]}")
        return 1
    print(f"baseline green\n")

    survived: list[Mutant] = []
    for m in MUTANTS:
        if src.count(m.old) != 1:
            print(f"SKIP  {m.name}\n      anchor matched {src.count(m.old)}x (must be 1)")
            survived.append(m)
            continue
        TARGET.write_text(src.replace(m.old, m.new, 1))
        code, out = run(["cargo", "test", "-p", "commons-store", "--test", TEST], REPO)
        TARGET.write_text(src)
        killed = code != 0
        note = ""
        if not killed:
            note = "  <-- SURVIVED"
            survived.append(m)
        # Which test caught it, so the report says the mutant died for a reason.
        names = re.findall(r"test (\S+) \.\.\. FAILED", out)
        print(f"{'KILL ' if killed else 'LIVE '} {m.name}")
        print(f"      {m.why}")
        if names:
            print(f"      caught by: {', '.join(sorted(set(names))[:3])}")

    print(f"\n{len(MUTANTS) - len(survived)}/{len(MUTANTS)} killed")
    for m in survived:
        print(f"  survived: {m.name}")
    if not keep:
        TARGET.write_text(src)
    return 1 if survived else 0


if __name__ == "__main__":
    raise SystemExit(main())
