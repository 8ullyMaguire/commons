#!/usr/bin/env python3
"""Regenerate the SQLite migration mirror from the Postgres one.

The Postgres file is the source of truth. This script copies it and rewrites
the header, leaving the DDL identical -- with one exception, described below.

Engine-specific statements
--------------------------
SQLite is not Postgres. It has no `ALTER COLUMN ... DROP NOT NULL`, and changing
a column's nullability means the twelve-step table rebuild. Rather than letting
the two files drift, a Postgres migration may mark a statement as
engine-specific:

    --:sqlite 0002_x.sqlite.sql
    ALTER TABLE appearance ALTER COLUMN cluster_id DROP NOT NULL;
    --:end

Everything between the markers is replaced in the SQLite mirror by the contents
of the named file, and the markers are stripped. This keeps the schema's
*meaning* stated once, in the Postgres file, with the engine's own syntax
spelled out where an engineer applying it will look. The parity test compares
table and column sets across the two engines, so a rewrite that changed the
shape would still fail there.

Run after editing any migration under migrations/postgres/:

    python3 scripts/sync-migrations.py

Then `cargo test -p commons-store migration_parity` proves the result.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MIGRATIONS = ROOT / "crates" / "commons-store" / "migrations"

# The Postgres header, from the first line up to the first blank line that ends
# it. Matching on the whole prose was brittle: the header was reworded once and
# the old pattern silently stopped matching, which made the script copy the
# Postgres header verbatim into the SQLite mirror and emit a warning nobody read.
# Anchoring on "the comment block before the first CREATE TABLE" is stable
# against rewording.
PG_HEADER = re.compile(r"\A(--[^\n]*\n)+", re.M)

# Written per file, with the name interpolated. Hardcoding 0001's text here was
# a real bug: every later mirror came out headed "0001_core.sql - Commons initial
# schema", and sqlx identifies a migration by its version number, not its prose,
# so the mirror of 0003 was applied with 0001's identity and the new column did
# not exist. The symptom was a query error naming a column that the migration
# plainly added.
SQLITE_HEADER = """-- {name} - SQLite mirror (Postgres: {name}).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/{name} and re-run that script.
-- T-P0-007's parity test fails if the two files' table sets ever diverge.
--
-- Portable-SQL rules in force (plan section 0.4):
--   * ids are TEXT
--   * timestamps are ISO-8601 UTC TEXT, so comparison and sort need no
--     timezone function and both engines agree
--   * no vector columns; embeddings live in a sidecar ANN file
--   * booleans are INTEGER 0|1 here and BOOLEAN in Postgres; the store crate
--     hides the difference and no query writes a literal
--   * foreign keys need `PRAGMA foreign_keys = ON` per connection, which the
--     store crate sets at open time
"""


# `--:sqlite <file>` ... `--:end`
SQLITE_BLOCK = re.compile(r"^--:sqlite\s+(\S+)\s*\n(.*?)^--:end\s*\n", re.S | re.M)


def expand_sqlite_blocks(text: str, pg_file: Path) -> str:
    """Replace each `--:sqlite` block with the SQLite form of that statement."""

    def sub(m: re.Match[str]) -> str:
        name = m.group(1)
        side = pg_file.parent / name
        if not side.exists():
            print(
                f"  error: {pg_file.name} marks a SQLite block reading {name},"
                f" which does not exist",
                file=sys.stderr,
            )
            raise SystemExit(1)
        body = side.read_text().rstrip("\n")
        return f"-- SQLite form of the statement above ({name}).\n{body}\n\n"

    return SQLITE_BLOCK.sub(sub, text)


def sync() -> int:
    pg_dir = MIGRATIONS / "postgres"
    lite_dir = MIGRATIONS / "sqlite"
    lite_dir.mkdir(parents=True, exist_ok=True)

    changed = 0
    for pg_file in sorted(pg_dir.glob("*.sql")):
        target = lite_dir / pg_file.name
        text = pg_file.read_text()
        header = SQLITE_HEADER.format(name=pg_file.name).rstrip("\n")
        # A lambda, not a replacement string: `re.sub` would otherwise read the
        # backslashes in the header as escapes and mangle them.
        body = PG_HEADER.sub(lambda _m: header, text, count=1)
        if body == text:
            print(
                f"  warn: no header block matched in {pg_file.name}",
                file=sys.stderr,
            )
        body = expand_sqlite_blocks(body, pg_file)
        if not target.exists() or target.read_text() != body:
            target.write_text(body)
            changed += 1
            print(f"  wrote {target.relative_to(ROOT)}")

    print(f"synced {changed} file(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(sync())
