#!/usr/bin/env python3
"""Check the schema against the data-model doc (P0-045, NFR-52).

Diffs the tables created by the migrations against the table list
in docs/03-ARCHITECTURE/04-data-model.md (`## Tables` section)
and fails (exit 1) on drift:

  - a table created by a migration but missing from the doc
    (the failure the ticket names: a table exists in the db
    but not in the doc), or
  - a table listed in the doc that no migration creates (the doc
    is ahead of the schema — also drift, reported separately).

Static only: it parses SQL files, it never connects to a database,
so it runs in CI without postgres. `_sqlx_migrations` is excluded
(sqlx-internal, never a product table and never in the doc).

Usage:
  check_schema_docs.py [--migrations DIR] [--doc PATH]
Defaults: migrations/, docs/03-ARCHITECTURE/04-data-model.md
(relative to the repo root, the script's parent's parent).
Exit 0 when the two lists agree, 1 on drift.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# sqlx-internal bookkeeping table: created by the migrator, never
# a product table, never listed in the data-model doc.
IGNORED_TABLES = frozenset({"_sqlx_migrations"})

CREATE_TABLE_RE = re.compile(
    r"CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?\"?([A-Za-z_][A-Za-z0-9_]*)\"?",
    re.IGNORECASE,
)
DOC_TABLE_RE = re.compile(r"^\*\*([A-Za-z_][A-Za-z0-9_]*)\*\*")


def parse_migration_tables(migrations_dir: Path) -> set[str]:
    """Table names created by the up migrations.

    Every `*.sql` file except `*.down.sql` (the down files DROP
    tables; the 0001 file is up-only and has no `.up.sql` suffix).
    """
    tables: set[str] = set()
    for path in sorted(migrations_dir.glob("*.sql")):
        if path.name.endswith(".down.sql"):
            continue
        tables.update(
            m.group(1) for m in CREATE_TABLE_RE.finditer(path.read_text())
        )
    return tables - IGNORED_TABLES


def parse_doc_tables(doc_path: Path) -> set[str]:
    """Table names in the `## Tables` section of the data-model doc.

    Each entry is a `**<name>** — ...` line; the section ends at
    the next `## ` heading. Tables named anywhere else (ERD,
    retention notes, index list) are not counted.
    """
    text = doc_path.read_text()
    tables: set[str] = set()
    in_tables = False
    for line in text.splitlines():
        if line.startswith("## "):
            in_tables = line.strip() == "## Tables"
            continue
        if in_tables:
            if m := DOC_TABLE_RE.match(line):
                tables.add(m.group(1))
    return tables


def diff(
    migration_tables: set[str], doc_tables: set[str]
) -> tuple[set[str], set[str]]:
    """(in migrations but not in doc, in doc but not in migrations)."""
    return migration_tables - doc_tables, doc_tables - migration_tables


def main(argv: list[str] | None = None) -> int:
    repo_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--migrations", type=Path, default=repo_root / "migrations"
    )
    parser.add_argument(
        "--doc",
        type=Path,
        default=repo_root / "docs/03-ARCHITECTURE/04-data-model.md",
    )
    args = parser.parse_args(argv)

    migration_tables = parse_migration_tables(args.migrations)
    doc_tables = parse_doc_tables(args.doc)
    missing_in_doc, missing_in_migrations = diff(migration_tables, doc_tables)

    if not missing_in_doc and not missing_in_migrations:
        print(
            f"schema docs OK: {len(migration_tables)} tables in migrations "
            f"and in docs/03-ARCHITECTURE/04-data-model.md"
        )
        return 0

    if missing_in_doc:
        print(
            "DRIFT: tables created by migrations but missing from the "
            f"data-model doc: {sorted(missing_in_doc)}",
            file=sys.stderr,
        )
    if missing_in_migrations:
        print(
            "DRIFT: tables listed in the data-model doc but created by "
            f"no migration: {sorted(missing_in_migrations)}",
            file=sys.stderr,
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
