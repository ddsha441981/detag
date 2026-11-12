"""Unit tests for scripts/env_check.py (P0-011, NFR-52).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

import tempfile
import unittest
from pathlib import Path

from env_check import (
    check,
    code_vars,
    expand_slash_group,
    parse_example,
    runbook_vars,
)


class TestSlashGroupExpansion(unittest.TestCase):
    def test_ttl_group(self):
        self.assertEqual(
            expand_slash_group("PF_RESULTS_TTL_FREE/PRO/BUSINESS_H"),
            {
                "PF_RESULTS_TTL_FREE_H",
                "PF_RESULTS_TTL_PRO_H",
                "PF_RESULTS_TTL_BUSINESS_H",
            },
        )

    def test_plain_token_unchanged(self):
        self.assertEqual(expand_slash_group("PF_JWT_SECRET"), {"PF_JWT_SECRET"})


class TestParseExample(unittest.TestCase):
    def test_keys_and_empty_values(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / ".env.example"
            path.write_text(
                "# comment\nPF_A=1\nPF_B=\nNOT_PF=2\nPF_C='quoted'\n\n",
                encoding="utf-8",
            )
            self.assertEqual(parse_example(path), {"PF_A", "PF_B", "PF_C"})


class TestCheck(unittest.TestCase):
    def _write(self, tmp: Path, example: str, runbook: str, code: str):
        example_path = tmp / ".env.example"
        example_path.write_text(example, encoding="utf-8")
        runbook_path = tmp / "runbook.md"
        runbook_path.write_text(
            "# Runbook\n\n## Environment variables (PF_*)\n\n"
            + runbook
            + "\n\n## Other\n",
            encoding="utf-8",
        )
        code_dir = tmp / "crates"
        (code_dir / "core" / "src").mkdir(parents=True)
        (code_dir / "core" / "src" / "lib.rs").write_text(code, encoding="utf-8")
        return example_path, runbook_path, code_dir

    def test_green_when_sets_agree(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            example, runbook, code_dir = self._write(
                tmp,
                "PF_DATABASE_URL=x\nPF_NEW_VAR=y\n",
                "| `PF_DATABASE_URL` | api | x |\n| `PF_NEW_VAR` | api | y |\n",
                'let url = env::var("PF_DATABASE_URL");\n'
                'let n = env::var("PF_NEW_VAR");\n',
            )
            errors, _ = check(example, runbook, [code_dir])
            self.assertEqual(errors, [])

    def test_missing_from_example(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            example, runbook, code_dir = self._write(
                tmp,
                "PF_DATABASE_URL=x\n",
                "| `PF_DATABASE_URL` | api | x |\n",
                'let url = env::var("PF_DATABASE_URL");\n'
                'let s = env::var("PF_STRIPE_SECRET_KEY");\n',
            )
            errors, _ = check(example, runbook, [code_dir])
            self.assertEqual(len(errors), 1)
            self.assertIn("PF_STRIPE_SECRET_KEY", errors[0])

    def test_undocumented_in_example(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            example, runbook, code_dir = self._write(
                tmp,
                "PF_DATABASE_URL=x\nPF_MYSTERY=y\n",
                "| `PF_DATABASE_URL` | api | x |\n",
                'let url = env::var("PF_DATABASE_URL");\n',
            )
            errors, _ = check(example, runbook, [code_dir])
            self.assertEqual(len(errors), 1)
            self.assertIn("PF_MYSTERY", errors[0])

    def test_star_pattern_skipped_and_slash_group_expanded(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            runbook_path = tmp / "runbook.md"
            runbook_path.write_text(
                "## Environment variables\n\n"
                "| `PF_RESULTS_TTL_FREE/PRO/BUSINESS_H` | cleanup | 6/24/72 |\n"
                "| `PF_RATELIMIT_*` | api | defaults |\n",
                encoding="utf-8",
            )
            self.assertEqual(
                runbook_vars(runbook_path),
                {
                    "PF_RESULTS_TTL_FREE_H",
                    "PF_RESULTS_TTL_PRO_H",
                    "PF_RESULTS_TTL_BUSINESS_H",
                },
            )

    def test_code_vars_finds_string_literals(self):
        with tempfile.TemporaryDirectory() as tmp:
            code_dir = Path(tmp) / "crates"
            (code_dir / "a").mkdir(parents=True)
            (code_dir / "a" / "x.rs").write_text(
                'env::var("PF_JWT_SECRET")\n// PF_NOT_A_REAL_REF in a comment\n',
                encoding="utf-8",
            )
            # Comments are scanned too (conservative: a mentioned var must
            # exist in .env.example); both tokens are therefore reported.
            self.assertEqual(
                code_vars([code_dir]), {"PF_JWT_SECRET", "PF_NOT_A_REAL_REF"}
            )


if __name__ == "__main__":
    unittest.main()
