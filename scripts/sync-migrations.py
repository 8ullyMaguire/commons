#!/usr/bin/env python3
"""Regenerate the SQLite migration mirror from the Postgres one.

The Postgres file is the source of truth. This script copies it and rewrites
the header and the engine-specific notes, leaving the DDL identical — SQLite
accepts everything here except the Postgres-only comment about native types,
which the header already covers.

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

PG_HEADER_OLD = re.compile(
    r"^-- 0001_core\.sql.*?\n-- store crate\s*\n--\s+hides the difference and no query writes a literal",
    re.S | re.M,
)

SQLITE_HEADER = """-- 0001_core.sql - Commons initial schema (SQLite).
--
-- GENERATED from the Postgres file by scripts/sync-migrations.py. Do not
-- hand-edit: edit migrations/postgres/0001_core.sql and re-run that script.
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


def sync() -> int:
    pg_dir = MIGRATIONS / "postgres"
    lite_dir = MIGRATIONS / "sqlite"
    lite_dir.mkdir(parents=True, exist_ok=True)

    changed = 0
    for pg_file in sorted(pg_dir.glob("*.sql")):
        target = lite_dir / pg_file.name
        text = pg_file.read_text()
        body = PG_HEADER_OLD.sub(SQLITE_HEADER.rstrip("\n"), text, count=1)
        if body == text:
            print(f"  warn: no header block matched in {pg_file.name}", file=sys.stderr)
        if not target.exists() or target.read_text() != body:
            target.write_text(body)
            changed += 1
            print(f"  wrote {target.relative_to(ROOT)}")

    print(f"synced {changed} file(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(sync())
