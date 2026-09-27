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
  * **named path is a STUB** -- the path exists and is empty or a single line of
    doc comment. See `is_stub` for why a file merely existing is not evidence.

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


def is_stub(p: Path) -> bool:
    """Does this path exist without containing any work?

    **Existence is not evidence.** A crate whose `lib.rs` is one line of `//!`
    doc comment compiles, joins the workspace, and passes a full test suite --
    it is a green build that produces nothing. That is the shape a placeholder
    takes, and a checker that only asks "does the file exist" reports it as
    done, which is worse than reporting nothing: it is a false claim generated
    by the tool whose job is to prevent false claims.

    Two cases, both about *substance* rather than size:

      * A single file whose every non-blank, non-comment line is nothing --
        a `.rs`/`.ts`/`.py` file with no code in it.
      * A directory whose files are all like that.

    The threshold is deliberately "no code at all" rather than "fewer than N
    lines". A genuinely small module -- an enum, a newtype, a three-line
    function -- is real work, and a line count would eventually classify it as
    a stub and invite someone to delete it.

    Comment-only is the right boundary because that is exactly what a
    placeholder is: someone wrote the header that says what the file *would*
    hold, and stopped.
    """
    if p.is_file():
        return _is_stub_file(p)
    if p.is_dir():
        # **Manifests and lockfiles are not code.** A crate directory whose only
        # real content is `Cargo.toml` is a placeholder that has been wired into
        # the workspace -- which is exactly `commons-client`, whose `lib.rs` is
        # one line of `//!` and whose `Cargo.toml` lists nine dependencies. A
        # directory check that counts the manifest as substance reports that as
        # built, and it is the shape a half-created crate always takes.
        #
        # So the test is over files that could *hold* code, and the manifest is
        # excluded for the same reason the file check is not a line count: a
        # manifest proves the crate was declared, not that anything was written.
        code = [
            f
            for f in p.rglob("*")
            if f.is_file()
            and "__pycache__" not in f.parts
            and f.name
            not in ("Cargo.toml", "Cargo.lock", "package.json", "tsconfig.json")
            and f.suffix not in (".md", ".txt", ".yml", ".yaml", ".json", ".toml")
        ]
        # A directory with no code at all is a stub, and `all()` on an empty
        # sequence is True, so this reads correctly without a special case --
        # but saying it is clearer than relying on it.
        return all(_is_stub_file(f) for f in code) if code else True
    return False


def _is_stub_file(p: Path) -> bool:
    try:
        text = p.read_text(encoding="utf-8", errors="replace")
    except OSError:
        # Unreadable is not the same as empty, and calling it a stub would be a
        # claim this function cannot support.
        return False
    for line in text.splitlines():
        s = line.strip()
        if not s:
            continue
        if s.startswith(("//", "#", "*", "/*", "*/", "<!--", "--")):
            continue
        return False
    return True


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
        # A glob is satisfied by any match; a literal path by existing. Both
        # then go through `is_stub`, because existing is not the same as built.
        missing = []
        stubs = []
        for p in paths:
            if any(ch in p for ch in "*?"):
                matches = list(REPO.glob(p.lstrip("./")))
                if not matches:
                    missing.append(p)
                elif all(is_stub(m) for m in matches):
                    stubs.append(p)
            else:
                target = REPO / p.lstrip("./")
                if not target.exists():
                    missing.append(p)
                elif is_stub(target):
                    stubs.append(p)
        present = [p for p in paths if p not in missing and p not in stubs]
        rows.append(
            {
                **t,
                "paths": paths,
                "missing": missing,
                "stubs": stubs,
                "present": present,
            }
        )

    claimed = [r for r in rows if r["claimed"]]
    false_claims = [r for r in claimed if r["missing"]]
    stub_claims = [r for r in claimed if r["stubs"] and not r["missing"]]
    unmarked = [r for r in rows if not r["claimed"] and r["present"]]
    # A ticket whose named path exists but holds no code. Neither built nor
    # unbuilt: the file is there and the work is not, which is the one state
    # the old "unmarked but present" bucket counted as progress.
    stubbed = [r for r in rows if r["stubs"] and r["present"]]
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
    print(f"  ...claiming a file that is a STUB: {len(stub_claims)}")
    print(f"not claimed, but the file is there: {len(unmarked)}")
    print(f"not claimed, and the file is a stub: {len(stubbed)}")
    print(f"not claimed, nothing on disk: {len(unstarted)}")

    if false_claims:
        print("\nCLAIMED DONE, FILE ABSENT -- the expensive failure:")
        for r in false_claims:
            print(f"  {r['id']}  {r['title'][:52]}")
            for p in r["missing"]:
                print(f"      missing: {p}")
    if stub_claims:
        print("\nCLAIMED DONE, FILE IS A STUB -- worse than absent, because it passes a build:")
        for r in stub_claims:
            print(f"  {r['id']}  {r['title'][:52]}")
            for p in r["stubs"]:
                print(f"      stub: {p}")
    if stubbed:
        print("\nSTUB ON DISK -- the file exists and contains no code:")
        for r in stubbed:
            print(f"  {r['id']}  {r['title'][:52]}")
            for p in r["stubs"]:
                print(f"      stub: {p}")
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
            elif r["claimed"] and r["stubs"]:
                flag = "STUB"
            elif not r["claimed"] and r["present"]:
                flag = "unmarked"
            print(f"  {flag:9} {r['id']}  {r['title'][:56]}")

    # A stub claim exits 1 for the same reason an absent one does, and the
    # reason is worse: a missing file fails a build eventually, while a stub
    # compiles, passes every test, and ships.
    return 1 if (false_claims or stub_claims) else 0


if __name__ == "__main__":
    sys.exit(main())
