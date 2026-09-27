#!/usr/bin/env python3
"""Measure the `stashapp` ecosystem, from the GitHub API, into a JSON fixture.

Phase 11 is the last phase, and every number in its plan and its brief came from
here. The first draft of those numbers was wrong in four places (729 YAML / 155
Python / 982 files / 12 themes, where the truth was 728 / 163 / 981 / 11) and
nothing failed, because the numbers were prose in a comment rather than
something anybody recomputed.

So they are a fixture, and this script regenerates it. T-P11-001 turns the
fixture into assertions; until then it is the check that keeps the documents
honest.

    ./scripts/measure-ecosystem.py                 # print the summary
    ./scripts/measure-ecosystem.py --write         # rewrite the fixture
    ./scripts/measure-ecosystem.py --check         # fail if it is stale

`--check` is what belongs in CI. It is deliberately a *staleness* check rather
than a *correctness* check: upstream pushes daily, so the numbers WILL change,
and what must not happen is the documents silently disagreeing with reality.

The authoritative listing is `orgs/stashapp/repos`, not a repository search. A
search for the org missed `website` and undercounted the org by one; the orgs
endpoint is the listing that cannot.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURE = REPO_ROOT / "docs" / "ecosystem-2026-09-27.json"

ORG = "stashapp"

# What each repository is TO THIS PROJECT, which is the one classification that
# matters and the one no API returns. `work` is adapted here, `read` is consulted,
# `consumed` is an input format, `out` is deliberately not a target.
SCOPE = {
    "CommunityScrapers": ("work", "the declarative scraper corpus"),
    "CommunityScripts": ("work", "plugins, themes, userscripts"),
    "stash-box": ("read", "a service to be a client of; MIT; not an artifact to adapt"),
    "stash": ("out", "the reference implementation"),
    "Stash-Docs": ("read", "behavioural specification; read, never port"),
    "StashDB-Docs": ("out", "documentation for a different service"),
    "website": ("out", "marketing"),
    "plugins-repo-template": ("consumed", "the source-index format T-P11-001 reads"),
    "scrapers-repo-template": ("consumed", "the source-index format T-P11-001 reads"),
    "StashServer": ("out", "archived pre-2019 Ruby server"),
    "StashFrontend": ("out", "archived pre-2019 TypeScript client"),
    "StashOSX": ("out", "archived pre-2019 Swift client"),
    "metadata-api-discuss": ("read", "dormant issue threads; historical context"),
    ".github": ("out", "org defaults"),
}

BRANCH = {"CommunityScrapers": "master", "CommunityScripts": "main"}


def gh(*args: str) -> Any:
    """One `gh api` call, as parsed JSON. Fails loudly rather than guessing."""
    out = subprocess.run(
        ["gh", "api", *args], capture_output=True, text=True, check=False
    )
    if out.returncode != 0:
        sys.exit(f"gh api {' '.join(args)} failed:\n{out.stderr.strip()}")
    return json.loads(out.stdout)


def blobs(repo: str, ref: str) -> list[str]:
    tree = gh(f"repos/{ORG}/{repo}/git/trees/{ref}?recursive=1")
    if tree.get("truncated"):
        # A truncated tree silently undercounts, which is the exact failure this
        # whole script exists to prevent.
        sys.exit(f"{repo}@{ref}: the tree came back TRUNCATED; counts would be wrong")
    return [e["path"] for e in tree["tree"] if e["type"] == "blob"]


def under(paths: list[str], prefix: str, segments_below: int) -> list[str]:
    """Files under `prefix` with at least `segments_below` path segments below it.

    Written as an absolute segment count rather than a difference, because the
    difference version was wrong twice in a row: it counted 231 YAML scrapers
    instead of 728, then 0, and each time it produced a confident number rather
    than an error. `scrapers/<site>/<file>.yml` is three segments; `plugins/<dir>/<file>`
    is three; `lib/<file>` is two.

    `segments_below` counts the segments AFTER the prefix, so the prefix itself
    is not included.
    """
    return [
        p
        for p in paths
        if p.startswith(prefix) and len(p[len(prefix):].split("/")) >= segments_below
    ]


def measure() -> dict:
    org = gh(f"orgs/{ORG}/repos?per_page=100&type=public")
    repos = {}
    for r in org:
        name = r["name"]
        if name not in SCOPE:
            # A repository we have never classified. Refusing to skip it silently
            # is the whole point: a new repo must be a decision, not an omission.
            sys.exit(
                f"{ORG}/{name} is not classified in SCOPE. Decide whether it is "
                f"work, read, consumed, or out of scope, and record why."
            )
        kind, why = SCOPE[name]
        repos[name] = {
            "scope": kind,
            "reason": why,
            "stars": r["stargazers_count"],
            "forks": r["forks_count"],
            "language": r["language"],
            "archived": r["archived"],
            "pushed_at": r["pushed_at"][:10],
            "license": (r.get("license") or {}).get("spdx_id"),
            "default_branch": r["default_branch"],
        }

    cs = blobs("CommunityScrapers", BRANCH["CommunityScrapers"])
    cm = blobs("CommunityScripts", BRANCH["CommunityScripts"])

    plugins = under(cm, "plugins/", 2)
    themes = under(cm, "themes/", 2)
    userscripts = under(cm, "userscripts/", 2)

    return {
        "measured": subprocess.run(
            ["date", "-Iseconds"], capture_output=True, text=True
        ).stdout.strip(),
        "org": ORG,
        "repo_count": len(repos),
        "repos": repos,
        "CommunityScrapers": {
            "ref": BRANCH["CommunityScrapers"],
            "total_files": len(cs),
            # `scrapers/` holds BOTH layouts: a flat `scrapers/<Site>.yml` and a
            # grouped `scrapers/<Site>/<Site>.yml`. 497 sit at the top level and
            # 231 in a subfolder, so anything assuming one layout silently reads
            # a third of the corpus. Everything below is therefore counted at
            # "one or more segments below `scrapers/`", not at a fixed depth --
            # a fixed depth is how the first version of this script reported 231
            # YAML scrapers and a confident 484-file total.
            "scrapers_dir_files": len(under(cs, "scrapers/", 1)),
            "scrapers_yaml": len([p for p in under(cs, "scrapers/", 1) if p.endswith(".yml")]),
            "scrapers_python": len([p for p in under(cs, "scrapers/", 1) if p.endswith(".py")]),
            "scrapers_markdown": len([p for p in under(cs, "scrapers/", 1) if p.endswith(".md")]),
            "scrapers_ruby": len([p for p in under(cs, "scrapers/", 1) if p.endswith(".rb")]),
            "scrapers_yaml_flat": len([p for p in under(cs, "scrapers/", 1) if p.endswith(".yml") and p.count("/") == 1]),
            "scrapers_yaml_grouped": len([p for p in under(cs, "scrapers/", 1) if p.endswith(".yml") and p.count("/") == 2]),
            "scraper_site_dirs": len({p.split("/")[1] for p in under(cs, "scrapers/", 2) if p.count("/") == 2}),
            "py_common_files": len([p for p in cs if "py_common" in p]),
            "lib_files": len(under(cs, "lib/", 1)),
        },
        "CommunityScripts": {
            "ref": BRANCH["CommunityScripts"],
            "total_files": len(cm),
            "plugin_files": len(plugins),
            "plugin_dirs": len({p.split("/")[1] for p in plugins}),
            "plugin_python": len([p for p in plugins if p.endswith(".py")]),
            "plugin_markdown": len([p for p in plugins if p.endswith(".md")]),
            "plugin_yaml": len([p for p in plugins if p.endswith(".yml")]),
            "plugin_js": len([p for p in plugins if p.endswith(".js")]),
            "theme_files": len(themes),
            "theme_dirs": len({p.split("/")[1] for p in themes}),
            "theme_css": len([p for p in themes if p.endswith(".css")]),
            "userscript_dirs": len({p.split("/")[1] for p in userscripts}),
            "userscript_files": len(userscripts),
            "archived_files": len(under(cm, "archive/", 1)),
        },
    }


def summary(m: dict) -> str:
    c, s = m["CommunityScrapers"], m["CommunityScripts"]
    return f"""\
{m['org']}: {m['repo_count']} public repositories
  {c['scrapers_dir_files']} files under scrapers/ -- {c['scrapers_yaml']} YAML \
({c['scrapers_yaml_flat']} flat + {c['scrapers_yaml_grouped']} in {c['scraper_site_dirs']} site dirs), \
{c['scrapers_python']} Python, {c['scrapers_markdown']} md, {c['scrapers_ruby']} rb
    {c['py_common_files']} py_common files, {c['lib_files']} in lib/
  {s['plugin_files']} plugin files across {s['plugin_dirs']} directories \
({s['plugin_python']} py, {s['plugin_markdown']} md, {s['plugin_yaml']} yml, {s['plugin_js']} js)
  {s['theme_dirs']} theme directories, {s['theme_files']} files, {s['theme_css']} css
  {s['userscript_dirs']} userscripts, {s['archived_files']} archived files
"""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--write", action="store_true", help="rewrite the fixture")
    ap.add_argument("--check", action="store_true", help="fail if the fixture is stale")
    args = ap.parse_args()

    if os.environ.get("GITHUB_TOKEN") is None and not _gh_authed():
        print("note: gh is unauthenticated; the API is rate-limited", file=sys.stderr)

    m = measure()
    print(summary(m), end="")

    # Everything but the timestamp, so a re-measure on the same day is not a diff.
    comparable = {k: v for k, v in m.items() if k != "measured"}

    if args.write:
        FIXTURE.write_text(json.dumps(m, indent=2, sort_keys=True) + "\n")
        print(f"wrote {FIXTURE.relative_to(REPO_ROOT)}")
        return 0

    if args.check:
        if not FIXTURE.exists():
            sys.exit(f"{FIXTURE} does not exist; run with --write")
        old = json.loads(FIXTURE.read_text())
        old_cmp = {k: v for k, v in old.items() if k != "measured"}
        if old_cmp != comparable:
            print("\nSTALE: upstream has moved since the fixture was written.", file=sys.stderr)
            print("The documents quote the OLD numbers. Either the change is", file=sys.stderr)
            print("irrelevant, or README.md, docs/HANDOFF.md, the plan and the", file=sys.stderr)
            print("Phase 11 brief now understate reality. Re-run with --write and", file=sys.stderr)
            print("update the prose, or record why the new numbers do not matter.", file=sys.stderr)
            return 1
        print("fixture is current")
        return 0

    return 0


def _gh_authed() -> bool:
    out = subprocess.run(["gh", "auth", "status"], capture_output=True, text=True, check=False)
    return out.returncode == 0


if __name__ == "__main__":
    sys.exit(main())
