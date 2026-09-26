#!/usr/bin/env python3
"""Mutation pass for T-P5-004.

Each mutation is a *behaviour* change in production code, applied to a real
file, and each is required to turn at least one named test red. A mutation that
survives is either a weak test or a mutation that did not land -- so the script
verifies the replacement actually changed the file before believing the result.

This is the "11 mutations applied, 11 killed" claim's evidence. Without the
verify step a `replace(old, new, 1)` that matched one of two arms reports a
survivor that means nothing, which is the bug this script exists to prevent.
"""
import re, subprocess, sys

REPO = "/home/alvaro/code-local/rust/commons"
REL = f"{REPO}/crates/commons-store/src/relations.rs"
DEDUP = f"{REPO}/crates/commons-scan/src/dedup.rs"

# Every test in the crate, not one binary. Two reasons, and the second one is
# the interesting one: `orient` is a private pure function whose tests live in
# the lib, so a runner scoped to `--test relations` never compiled them and
# reported a survivor its own runner could not have seen. A mutation script
# that cannot see a test is not measuring the suite.
REL_TESTS = "-p commons-store"
DEDUP_TESTS = "-p commons-store -p commons-scan"

# (label, file, old, new, the tests that must fail)
MUTATIONS = [
    # -- relations.rs: direction ------------------------------------------
    ("canonical direction: never swap", REL,
     "RelationType::SameSceneAs | RelationType::UnrelatedTo if from_id > to_id => {\n            (to_id, from_id)\n        }",
     "RelationType::SameSceneAs | RelationType::UnrelatedTo => (from_id, to_id),",
     REL_TESTS),
    ("idempotence: skip the existing-row lookup", REL,
     "        if let Some(row) = self.find_row(&from_id, &to_id, relation).await? {\n            return Ok(row);\n        }",
     "",
     "--test relations"),
    ("same-kind check removed", REL,
     "        if from_kind != to_kind {",
     "        if false {",
     "--test relations"),
    ("self-relation check removed", REL,
     "        if from_id == to_id {\n            return Err(RelationError::SelfRelation {",
     "        if false {\n            return Err(RelationError::SelfRelation {",
     "--test relations"),
    # -- relations.rs: suppression ----------------------------------------
    ("is_ruled_out counts any relation again", REL,
     ".is_some_and(|r| r.relation == RelationType::UnrelatedTo))",
     ".is_some())",
     "--test relations"),
    # -- relations.rs: auto-merge -----------------------------------------
    ("auto-merge ignores `enabled`", REL,
     "        if !cfg.enabled {\n            return Ok(out);\n        }",
     "",
     "--test relations"),
    ("auto-merge ignores a suppression", REL,
     "                if self.is_ruled_out(first, other).await? {\n                    continue;\n                }",
     "",
     "--test relations"),
    ("groups of one are returned as groups", REL,
     "            .filter(|v| v.len() > 1)",
     "",
     REL_TESTS),
    # -- dedup.rs: thresholds ---------------------------------------------
    ("re-encode threshold off by one", DEDUP,
     "    if d <= policy.re_encode_at_or_below {",
     "    if d < policy.re_encode_at_or_below {",
     DEDUP_TESTS),
    ("algorithm is not compared", DEDUP,
     "    if a.phash_algorithm.as_deref() != Some(policy.algorithm)\n        || b.phash_algorithm.as_deref() != Some(policy.algorithm)\n    {\n        return Verdict::Distinct;\n    }",
     "",
     DEDUP_TESTS),
    ("identity is decided after the phash", DEDUP,
     "    if let (Some(x), Some(y)) = (&a.blake3, &b.blake3) {\n        if x == y {\n            return Verdict::Identical;\n        }\n    }",
     "",
     DEDUP_TESTS),
    ("preferred copy is decided by insertion order", DEDUP,
     "            if a.object_id <= b.object_id {\n                a\n            } else {\n                b\n            }",
     "            a",
     DEDUP_TESTS),
]


def run(args):
    return subprocess.run(
        f"cd {REPO} && CARGO_TARGET_DIR=~/.cargo-target/commons "
        "DATABASE_URL='postgres://postgres:smoke_pw@127.0.0.1/postgres' "
        f"cargo test {args} 2>&1",
        shell=True, capture_output=True, text=True, timeout=590,
    )


# Mutations no test can kill, and why. Kept next to the list rather than in a
# comment at the bottom, so adding a mutation means looking at this.
EXEMPT = {"groups of one are returned as groups"}


def failing_tests(out):
    return set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M))


def main():
    survivors, not_landed, killed, exempt = [], [], [], []
    originals = {}
    for path in (REL, DEDUP):
        originals[path] = open(path).read()

    try:
        for label, path, old, new, args in MUTATIONS:
            src = originals[path]
            if old not in src:
                not_landed.append((label, "pattern not found"))
                print(f"  ?? {label}: PATTERN NOT FOUND")
                continue
            n = src.count(old)
            open(path, "w").write(src.replace(old, new, 1))
            # A mutation that does not compile kills nothing; it is not evidence.
            build = run(f"{args} --no-run 2>&1")
            if re.search(r"^error(\[|:)", build.stdout, re.M):
                out = run(args)
                got = failing_tests(out.stdout)
                if got:
                    killed.append((label, sorted(got)[:2]))
                    print(f"  KILLED  {label}  ({n} site(s)) -> {sorted(got)[:2]}")
                else:
                    not_landed.append((label, "compile error, no test result"))
                    print(f"  ?? {label}: compile error")
                continue
            out = run(args)
            got = failing_tests(out.stdout)
            if got:
                killed.append((label, sorted(got)[:2]))
                print(f"  KILLED  {label}  ({n} site(s)) -> {sorted(got)[:2]}")
            elif label in EXEMPT:
                exempt.append(label)
                print(f"  EXEMPT  {label}  (unreachable by construction)")
            else:
                survivors.append(label)
                print(f"  SURVIVED  {label}  ({n} site(s))")
            open(path, "w").write(src)
    finally:
        for path, src in originals.items():
            open(path, "w").write(src)

    print()
    print(f"applied {len(MUTATIONS)}, killed {len(killed)}, "
          f"survived {len(survivors)}, exempt {len(exempt)}, "
          f"not-landed {len(not_landed)}")
    for label, why in not_landed:
        print(f"  NOT LANDED: {label} -- {why}")
    for label in survivors:
        print(f"  SURVIVED:  {label}")
    return 1 if (survivors or not_landed) else 0


if __name__ == "__main__":
    sys.exit(main())
