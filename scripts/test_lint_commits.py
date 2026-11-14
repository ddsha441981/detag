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
# Issue ids in the ticket position (COMMITS-LINT-001): a CI-fix or
# schema-docs commit has no P-ticket, so it carries its issue id.
# The FR/NFR trace is still required — the id form is not a free pass.
VALID_CI_FAIL = (
    "fix(ops): CI-FAIL-004 replace dead MinIO with Garage S3\n"
    "\nMinIO's distribution is gone on every channel (NFR-52).\n"
)
VALID_SCHEMA_DOC = (
    "fix(docs): SCHEMA-DOC-001 regenerate schema docs after the migration\n"
    "\nThe drift check compares generated docs (NFR-52).\n"
)
# A flaky-test fix carries its FLAKY id (CI-FAIL-019): Commits #22
# failed on 8a65cd4's `test(core): fix FLAKY-002 ...` subject because
# the linter only accepted P-/CI-FAIL-/SCHEMA-DOC- ids.
VALID_FLAKY = (
    "test(core): FLAKY-002 slow_consumer_drops_safely race (NFR-52)\n"
    "\nDrain stale Progress events until Done arrives.\n"
)


class TestValidMessages(unittest.TestCase):
    def test_standard_feat_with_body(self):
        self.assertEqual(check_message(VALID), [])

    def test_nfr_trace_in_subject(self):
        self.assertEqual(check_message(VALID_NFR_ONLY), [])

    def test_fr_area_trace(self):
        self.assertEqual(check_message(VALID_FR_AREA), [])

    def test_ci_fail_id_in_ticket_position(self):
        self.assertEqual(check_message(VALID_CI_FAIL), [])

    def test_schema_doc_id_in_ticket_position(self):
        self.assertEqual(check_message(VALID_SCHEMA_DOC), [])

    def test_flaky_id_in_ticket_position(self):
        self.assertEqual(check_message(VALID_FLAKY), [])

    def test_flaky_id_needs_a_number(self):
        errors = check_message(
            "test(core): FLAKY slow_consumer_drops_safely race (NFR-52)\n"
        )
        self.assertTrue(any("must match" in e for e in errors))

    def test_ci_fail_id_still_needs_requirement_trace(self):
        errors = check_message(
            "fix(ops): CI-FAIL-004 replace dead MinIO with Garage S3\n"
        )
        self.assertTrue(any("requirement id" in e for e in errors))

    def test_ci_fail_id_needs_a_number(self):
        errors = check_message(
            "fix(ops): CI-FAIL replace dead MinIO with Garage S3 (NFR-52)\n"
        )
        self.assertTrue(any("must match" in e for e in errors))

    def test_wrong_issue_prefix_rejected(self):
        errors = check_message(
            "fix(ops): FAIL-004 replace dead MinIO with Garage S3 (NFR-52)\n"
        )
        self.assertTrue(any("must match" in e for e in errors))


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
