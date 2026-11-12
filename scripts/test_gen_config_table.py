"""Unit + drift tests for scripts/gen_config_table.py (P0-024, NFR-52).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
(The `just test-lint-commits` recipe runs this discovery for all
scripts tests, despite the name.)
"""

import tempfile
import unittest
from pathlib import Path

from gen_config_table import generate, parse_example_defaults, runbook_used_by

REPO_ROOT = Path(__file__).resolve().parent.parent

EXAMPLE = """# comment
PF_API_PORT=8080
PF_SMTP_HOST=localhost
PF_SMTP_USER=
PF_RESULTS_TTL_FREE_H=6
PF_RESULTS_TTL_PRO_H=24
PF_S3_ENDPOINT=http://localhost:9000
PF_S3_BUCKET=detag-dev
"""

RUNBOOK = """# Runbook

## Environment variables (PF_*)

| Var | Used by | Default (dev) | Notes |
| --- | --- | --- | --- |
| `PF_API_PORT` | api | 8080 | |
| `PF_SMTP_HOST`, `PF_SMTP_USER` | mailer | MailHog | |
| `PF_RESULTS_TTL_FREE/PRO_H` | cleanup | 6/24 | |
| `PF_S3_ENDPOINT`, `PF_S3_BUCKET` | storage | MinIO | |
| `PF_RATELIMIT_*` | api | per-plan defaults | |

## Something else
"""


def _write(tmp: Path, name: str, content: str) -> Path:
    path = tmp / name
    path.write_text(content, encoding="utf-8")
    return path


class TestParse(unittest.TestCase):
    def test_example_defaults_keep_values_and_empty(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(Path(tmp), ".env.example", EXAMPLE)
            defaults = parse_example_defaults(path)
            self.assertEqual(defaults["PF_API_PORT"], "8080")
            self.assertEqual(defaults["PF_SMTP_USER"], "")

    def test_runbook_used_by_expands_groups(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(Path(tmp), "runbook.md", RUNBOOK)
            used_by = runbook_used_by(path)
            self.assertEqual(used_by["PF_API_PORT"], "api")
            # Comma group: both vars share the row's used-by.
            self.assertEqual(used_by["PF_SMTP_HOST"], "mailer")
            self.assertEqual(used_by["PF_SMTP_USER"], "mailer")
            self.assertEqual(used_by["PF_S3_ENDPOINT"], "storage")
            self.assertEqual(used_by["PF_S3_BUCKET"], "storage")
            # Slash group expands to both vars.
            self.assertEqual(used_by["PF_RESULTS_TTL_FREE_H"], "cleanup")
            self.assertEqual(used_by["PF_RESULTS_TTL_PRO_H"], "cleanup")
            # Star patterns are not concrete vars.
            self.assertNotIn("PF_RATELIMIT_*", used_by)
            self.assertFalse(any(k.startswith("PF_RATELIMIT") for k in used_by))


class TestGenerate(unittest.TestCase):
    def _files(self, tmp: Path, example: str = EXAMPLE, runbook: str = RUNBOOK):
        return (
            _write(tmp, ".env.example", example),
            _write(tmp, "runbook.md", runbook),
        )

    def test_table_renders_name_default_used_by(self):
        with tempfile.TemporaryDirectory() as tmp:
            example, runbook = self._files(Path(tmp))
            markdown, errors = generate(example, runbook)
            self.assertEqual(errors, [])
            self.assertIn("| `PF_API_PORT` | `8080` | api |", markdown)
            self.assertIn("| `PF_SMTP_HOST` | `localhost` | mailer |", markdown)
            # Empty defaults render as (empty), not as bare backticks.
            self.assertIn("| `PF_SMTP_USER` | (empty) | mailer |", markdown)
            # Rows are sorted by name.
            names = [
                line.split("`")[1]
                for line in markdown.splitlines()
                if line.startswith("| `PF_")
            ]
            self.assertEqual(names, sorted(names))

    def test_missing_used_by_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            example, runbook = self._files(
                Path(tmp), example=EXAMPLE + "PF_ORPHAN=1\n"
            )
            _, errors = generate(example, runbook)
            self.assertEqual(len(errors), 1)
            self.assertIn("PF_ORPHAN", errors[0])

    def test_missing_example_entry_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            example, runbook = self._files(
                Path(tmp),
                runbook=RUNBOOK.replace(
                    "| `PF_API_PORT` | api | 8080 | |",
                    "| `PF_API_PORT` | api | 8080 | |\n"
                    "| `PF_NEW_VAR` | api | x | |",
                ),
            )
            _, errors = generate(example, runbook)
            self.assertEqual(len(errors), 1)
            self.assertIn("PF_NEW_VAR", errors[0])


class TestCommittedTable(unittest.TestCase):
    def test_committed_table_matches_generated(self):
        """Drift test: the committed doc must equal what the generator
        produces from the real .env.example + runbook. Fails the moment
        either source changes without regenerating."""
        example = REPO_ROOT / ".env.example"
        runbook = REPO_ROOT / "docs/06-DELIVERY/03-deploy-runbook.md"
        committed = REPO_ROOT / "docs/06-DELIVERY/04-config-reference.md"
        markdown, errors = generate(example, runbook)
        self.assertEqual(errors, [], f"generator errors: {errors}")
        self.assertTrue(
            committed.exists(),
            "no committed table — run `just gen-config-table`",
        )
        self.assertEqual(
            committed.read_text(encoding="utf-8"),
            markdown,
            "committed table is stale — run `just gen-config-table`",
        )


if __name__ == "__main__":
    unittest.main()
