#!/usr/bin/env python3
"""Generate ui/tokens/tokens.css from tokens.json, or check that they agree.

  ./build.py            rewrite tokens.css
  ./build.py --check    exit 1 if tokens.css is stale (use this in CI)

The point of --check is that the two files cannot drift: a token edited in the JSON and not regenerated is a
build failure, not a mystery on someone's screen six weeks later.
"""
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
LIGHT = """@media (prefers-reduced-motion: reduce){:root{--duration-fast:0ms;--duration:0ms}}
@media (prefers-color-scheme: light){:root{
  --bg:#f7f8f9; --surface:#ffffff; --surface-raised:#f1f3f5; --border:#dfe3e7; --border-strong:#c4cbd2;
  --text:#16191c; --text-muted:#5a646d; --text-faint:#8a939c;
}}"""


def render(tokens: dict) -> str:
    lines = [
        "/* GENERATED from tokens.json by build.py -- do not edit by hand. */",
        "/* Estate design tokens v%s. Use var(--role), never a literal colour. */" % tokens["meta"]["version"],
        ":root{",
    ]
    for group, entries in tokens.items():
        if group == "meta":
            continue
        lines.append("  /* %s */" % group)
        for key, spec in entries.items():
            lines.append("  --%s: %s;" % (key, spec["value"]))
    lines.append("}")
    lines.append(LIGHT)
    return "\n".join(lines) + "\n"


def main() -> int:
    tokens = json.loads((HERE / "tokens.json").read_text())
    wanted = render(tokens)
    target = HERE / "tokens.css"
    if "--check" in sys.argv:
        current = target.read_text() if target.exists() else ""
        if current != wanted:
            print("tokens.css is STALE against tokens.json -- run ui/tokens/build.py", file=sys.stderr)
            return 1
        print("tokens.css matches tokens.json (%d declarations)" % wanted.count("--"))
        return 0
    target.write_text(wanted)
    print("wrote tokens.css (%d lines)" % len(wanted.splitlines()))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
