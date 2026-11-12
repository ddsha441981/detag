"""Unit tests for scripts/lint_commits.py (P0-006, NFR-52).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
(CI runs this in .github/workflows/commits.yml.)
"""

import unittest

from lint_commits import check_message

VALID = (
    "feat(auth): P1-014 refresh token rotation with reuse detection\n"
    "\nImplements rotating refresh tokens (FR-A-02).\n"
)
VALID_NFR_ONLY = "chore(repo): P0-005 justfile task runner (NFR-52)\n"
VALID_FR_AREA = "feat(engine): P2-034 tiled LaMa inference with overlap blend (FR-D-05)\n"


class TestValidMessages(unittest.TestCase):
    def test_standard_feat_with_body(self):
        self.assertEqual(check_message(VALID), [])

    def test_nfr_trace_in_subject(self):
        self.assertEqual(check_message(VALID_NFR_ONLY), [])

    def test_fr_area_trace(self):
        self.assertEqual(check_message(VALID_FR_AREA), [])


class TestInvalidMessages(unittest.TestCase):
    def test_missing_ticket_id(self):
        errors = check_message("feat(auth): refresh token rotation (FR-A-02)\n")
        self.assertTrue(any("P<phase>-<num>" in e for e in errors))

    def test_missing_requirement_trace(self):
        errors = check_message("feat(auth): P1-014 refresh token rotation\n")
        self.assertTrue(any("requirement id" in e for e in errors))

    def test_missing_scope(self):
        errors = check_message("feat: P1-014 refresh token rotation (FR-A-02)\n")
        self.assertTrue(any("must match" in e for e in errors))

    def test_unknown_type(self):
        errors = check_message("wip(auth): P1-014 refresh token rotation (FR-A-02)\n")
        self.assertTrue(any("unknown type" in e for e in errors))

    def test_unknown_scope(self):
        errors = check_message("feat(frontend): P1-014 refresh token rotation (FR-A-02)\n")
        self.assertTrue(any("unknown scope" in e for e in errors))

    def test_subject_too_long(self):
        long_desc = "x" * 90
        errors = check_message(
            f"feat(auth): P1-014 {long_desc} (FR-A-02)\n"
        )
        self.assertTrue(any("max 100" in e for e in errors))

    def test_empty_message(self):
        self.assertTrue(check_message(""))

    def test_freeform_message(self):
        errors = check_message("fixed some stuff\n")
        self.assertTrue(any("must match" in e for e in errors))

    def test_all_spec_scopes_accepted(self):
        for scope in ("auth", "engine", "api", "worker", "web", "ops"):
            with self.subTest(scope=scope):
                msg = f"chore({scope}): P0-006 scope check (NFR-52)\n"
                self.assertEqual(check_message(msg), [])


if __name__ == "__main__":
    unittest.main()
