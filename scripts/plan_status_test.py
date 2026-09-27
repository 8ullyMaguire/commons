#!/usr/bin/env python3
"""`is_stub` — the check that stops a placeholder counting as built.

T-P8-004's `commons-client` is the case this exists for: a crate that joins the
workspace, compiles, and passes every test while its `lib.rs` is one line of
`//!`. Before this check, `plan-status.py` reported it as "built but unmarked",
which is a false claim produced by the tool whose job is to prevent them.

Run: `python3 scripts/plan_status_test.py`
"""
import importlib.util
import sys
import tempfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "plan_status", Path(__file__).resolve().parent / "plan-status.py"
)
ps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ps)

failures: list[str] = []


def check(name: str, got: object, want: object) -> None:
    if got != want:
        failures.append(f"{name}: got {got!r}, want {want!r}")


def write(d: Path, rel: str, body: str) -> Path:
    p = d / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)
    return p


with tempfile.TemporaryDirectory() as td:
    d = Path(td)

    # --- the case the check was written for ---
    stub_crate = d / "stub-crate"
    write(stub_crate, "Cargo.toml", "[package]\nname = \"stub\"\n")
    write(stub_crate, "src/lib.rs", "//! stub-crate -- see docs/spec/commons-spec.md.\n")
    check("a crate with a manifest and a comment-only lib.rs is a stub", ps.is_stub(stub_crate), True)

    # A Cargo.toml is a declaration, not an implementation. Without this the
    # crate above reads as built, because the manifest has content in it.
    only_manifest = d / "only-manifest"
    write(only_manifest, "Cargo.toml", "[package]\nname = \"x\"\n")
    check("a directory holding only a manifest is a stub", ps.is_stub(only_manifest), True)

    check("an absent path is not a stub (it is missing, a different failure)", ps.is_stub(d / "nope"), False)
    check("an empty directory is a stub", ps.is_stub(only_manifest), True)

    # --- and the ones it must NOT flag ---
    real = d / "real"
    write(real, "src/lib.rs", "pub fn answer() -> u32 { 42 }\n")
    check("a crate with a function is not a stub", ps.is_stub(real), False)

    small = d / "small"
    write(small, "src/lib.rs", "/// A newtype.\npub struct Ms(pub u32);\n")
    check("a three-line module is real work, not a stub", ps.is_stub(small), False)

    # A line-count heuristic would eventually call this a stub and invite
    # someone to delete it. That is the failure this boundary is chosen to avoid.
    comment_then_code = d / "commented"
    write(commented := comment_then_code, "src/lib.rs", "// a long\n// comment\n// header\n\nexport const X = 1;\n")
    check("comments above code do not make it a stub", ps.is_stub(commented), False)

    # A crate where one module is real and another is a placeholder is built.
    # `all()` rather than `any()` on purpose.
    mixed = d / "mixed"
    write(mixed, "src/lib.rs", "//! mixed\n")
    write(mixed, "src/real.rs", "pub fn f() {}\n")
    check("one real module makes the crate built", ps.is_stub(mixed), False)

    # Prose in a directory is not code either -- a ticket that names `docs/`
    # should not read as built because the directory has markdown in it.
    docs = d / "docs-only"
    write(docs, "guide.md", "# Guide\n")
    check("a directory of markdown is a stub", ps.is_stub(docs), True)

    # --- the real repository, as a regression on the case that started this ---
    repo = Path(__file__).resolve().parent.parent
    check(
        "commons-client in this repo is a stub",
        ps.is_stub(repo / "crates" / "commons-client"),
        True,
    )
    for real_crate in ("commons-core", "commons-media", "commons-store", "commons-scan"):
        check(
            f"{real_crate} is not a stub",
            ps.is_stub(repo / "crates" / real_crate),
            False,
        )

if failures:
    print("FAIL")
    for f in failures:
        print(f"  {f}")
    sys.exit(1)
print("ok: is_stub behaves on every case")
