"""Unit + drift tests for scripts/check_schema_docs.py (P0-045, NFR-52).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
(The `just test-lint-commits` recipe runs this discovery for all
scripts tests, despite the name.)
"""

import tempfile
import unittest
from pathlib import Path

from check_schema_docs import diff, parse_doc_tables, parse_migration_tables

REPO_ROOT = Path(__file__).resolve().parent.parent


class TestParseMigrations(unittest.TestCase):
    def test_create_table_variants(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "0001_a.up.sql").write_text(
                "CREATE TABLE users (id UUID PRIMARY KEY);\n"
                "CREATE TABLE IF NOT EXISTS feature_flags (\n"
                "    key TEXT PRIMARY KEY\n"
                ");\n"
            )
            (d / "0002_b.up.sql").write_text(
                'CREATE TABLE "jobs" (id UUID PRIMARY KEY);\n'
            )
            # Down files DROP tables — they must not contribute.
            (d / "0002_b.down.sql").write_text("DROP TABLE jobs;\n")
            # Up-only file (no .up.sql suffix, like 0001_feature_flags).
            (d / "0001_feature_flags.sql").write_text(
                "CREATE TABLE IF NOT EXISTS feature_flags (key TEXT PRIMARY KEY);\n"
            )
            self.assertEqual(
                parse_migration_tables(d),
                {"users", "feature_flags", "jobs"},
            )

    def test_sqlx_migrations_table_is_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "0001_a.up.sql").write_text(
                "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY);\n"
                "CREATE TABLE plans (id UUID PRIMARY KEY);\n"
            )
            self.assertEqual(parse_migration_tables(d), {"plans"})


class TestParseDoc(unittest.TestCase):
    def test_only_the_tables_section_counts(self):
        doc = """# Data Model

## ERD (textual)

users 1─* sessions

## Tables

**users** — id, email.
**jobs** — id, user_id.

## Indexes (minimum)

jobs(user_id, created_at desc)

## Retention & cleanup jobs

- artifacts: `expires_at` = now + plan TTL.
"""
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "04-data-model.md"
            p.write_text(doc)
            self.assertEqual(parse_doc_tables(p), {"users", "jobs"})


class TestDiff(unittest.TestCase):
    def test_both_directions(self):
        missing_doc, missing_mig = diff({"a", "b"}, {"b", "c"})
        self.assertEqual(missing_doc, {"a"})
        self.assertEqual(missing_mig, {"c"})

    def test_clean(self):
        self.assertEqual(diff({"a"}, {"a"}), (set(), set()))


class TestRealRepo(unittest.TestCase):
    def test_migrations_and_doc_agree(self):
        """The actual drift test: the live repo must be clean."""
        migration_tables = parse_migration_tables(REPO_ROOT / "migrations")
        doc_tables = parse_doc_tables(
            REPO_ROOT / "docs/03-ARCHITECTURE/04-data-model.md"
        )
        missing_doc, missing_mig = diff(migration_tables, doc_tables)
        self.assertEqual(missing_doc, set())
        self.assertEqual(missing_mig, set())
        self.assertEqual(len(migration_tables), 19)


if __name__ == "__main__":
    unittest.main()
