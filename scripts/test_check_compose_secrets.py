"""Tests for scripts/check_compose_secrets.py (P0-105)."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_compose_secrets as ccs  # noqa: E402


class TestClassifyValue(unittest.TestCase):
    def test_bare_reference(self) -> None:
        self.assertEqual(ccs.classify_value("${PF_JWT_SECRET}")[0], "bare")
        self.assertEqual(ccs.classify_value("\"${PF_JWT_SECRET}\"")[0], "bare")

    def test_placeholder_default(self) -> None:
        kind, default = ccs.classify_value("${PF_S3_SECRET_KEY:-minioadmin}")
        self.assertEqual((kind, default), ("placeholder", "minioadmin"))
        self.assertEqual(ccs.classify_value("dev-fixed")[0], "placeholder")
        self.assertEqual(ccs.classify_value("pf")[0], "placeholder")

    def test_literal(self) -> None:
        kind, default = ccs.classify_value("${PF_JWT_SECRET:-hunter2}")
        self.assertEqual((kind, default), ("literal", "hunter2"))
        self.assertEqual(ccs.classify_value("hunter2")[0], "literal")
        self.assertEqual(ccs.classify_value("")[0], "literal")


class TestCheckFile(unittest.TestCase):
    def _file(self, name: str, text: str) -> Path:
        tmp = Path(tempfile.mkdtemp())
        path = tmp / name
        path.write_text(text, encoding="utf-8")
        return path

    def test_strict_file_rejects_default(self) -> None:
        path = self._file(
            "compose.base.yml",
            "services:\n  api:\n    environment:\n"
            "      PF_JWT_SECRET: ${PF_JWT_SECRET:-dev-fixed}\n",
        )
        violations = ccs.check_file(path)
        self.assertEqual(len(violations), 1)
        self.assertIn("PF_JWT_SECRET", violations[0])

    def test_strict_file_accepts_bare_reference(self) -> None:
        path = self._file(
            "compose.base.yml",
            "services:\n  api:\n    environment:\n"
            "      PF_JWT_SECRET: ${PF_JWT_SECRET}\n"
            "      PF_S3_SECRET_KEY: ${PF_S3_SECRET_KEY}\n",
        )
        self.assertEqual(ccs.check_file(path), [])

    def test_strict_file_rejects_plain_literal(self) -> None:
        path = self._file(
            "compose.prod.yml",
            "services:\n  api:\n    environment:\n"
            "      PF_JWT_SECRET: hunter2\n",
        )
        self.assertEqual(len(ccs.check_file(path)), 1)

    def test_dev_file_accepts_placeholder(self) -> None:
        path = self._file(
            "compose.dev.yml",
            "services:\n  api:\n    environment:\n"
            "      PF_JWT_SECRET: dev-fixed\n"
            "      PF_POSTGRES_PASSWORD: pf\n",
        )
        self.assertEqual(ccs.check_file(path), [])

    def test_dev_file_rejects_real_looking_literal(self) -> None:
        path = self._file(
            "compose.dev.yml",
            "services:\n  api:\n    environment:\n"
            "      PF_JWT_SECRET: hunter2\n",
        )
        self.assertEqual(len(ccs.check_file(path)), 1)

    def test_non_secret_vars_ignored(self) -> None:
        path = self._file(
            "compose.base.yml",
            "services:\n  postgres:\n    environment:\n"
            "      POSTGRES_USER: ${PF_POSTGRES_USER:-pf}\n"
            "      POSTGRES_DB: ${PF_POSTGRES_DB:-pf}\n",
        )
        self.assertEqual(ccs.check_file(path), [])

    def test_unknown_file_is_strict(self) -> None:
        path = self._file(
            "compose.staging.yml",
            "services:\n  api:\n    environment:\n"
            "      PF_JWT_SECRET: dev-fixed\n",
        )
        self.assertEqual(len(ccs.check_file(path)), 1)


class TestCheckRootLive(unittest.TestCase):
    def test_repo_compose_files_are_clean(self) -> None:
        root = Path(__file__).resolve().parent.parent
        self.assertEqual(ccs.check_root(root), [])


if __name__ == "__main__":
    unittest.main()
