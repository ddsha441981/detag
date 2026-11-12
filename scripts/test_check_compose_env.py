"""Unit tests for scripts/check_compose_env.py (P0-089).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

import tempfile
import unittest
from pathlib import Path

from check_compose_env import check

EXAMPLE = """\
PF_POSTGRES_USER=pf
PF_POSTGRES_PASSWORD=pf
PF_POSTGRES_DB=pf
PF_S3_ENDPOINT=http://localhost:9000
PF_S3_BUCKET=detag-dev
PF_S3_ACCESS_KEY=minioadmin
PF_S3_SECRET_KEY=minioadmin
PF_JWT_SECRET=dev-fixed
PF_DATABASE_URL=postgres://pf:pf@localhost:5432/pf
"""

GOOD = """\
name: detag

services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: ${PF_POSTGRES_USER:-pf}
      POSTGRES_PASSWORD: ${PF_POSTGRES_PASSWORD:-pf}
      POSTGRES_DB: ${PF_POSTGRES_DB:-pf}
    healthcheck:
      test:
        [
          "CMD-SHELL",
          "pg_isready -U $${POSTGRES_USER:-pf} -d $${POSTGRES_DB:-pf}",
        ]
  api:
    image: detag-api:latest
    environment:
      PF_DATABASE_URL: postgres://${PF_POSTGRES_USER:-pf}:${PF_POSTGRES_PASSWORD:-pf}@postgres:5432/${PF_POSTGRES_DB:-pf}
      PF_JWT_SECRET: ${PF_JWT_SECRET:-dev-fixed}
      PF_S3_ENDPOINT: ${PF_S3_ENDPOINT:-http://minio:9000}
  bootstrap:
    image: minio/mc:latest
    restart: "no"
    environment:
      # The container-side default legitimately differs from the
      # example's host-side default (P0-085) — see DIVERGENT_DEFAULTS.
      PF_S3_ENDPOINT: ${PF_S3_ENDPOINT:-http://minio:9000}
      PF_S3_BUCKET: ${PF_S3_BUCKET:-detag-dev}
      PF_S3_ACCESS_KEY: ${PF_S3_ACCESS_KEY:-minioadmin}
      PF_S3_SECRET_KEY: ${PF_S3_SECRET_KEY:-minioadmin}
"""


def _check(compose_text: str, example_text: str = EXAMPLE) -> list[str]:
    with tempfile.TemporaryDirectory() as d:
        ops_dir = Path(d) / "ops"
        ops_dir.mkdir()
        (ops_dir / "compose.base.yml").write_text(compose_text)
        example = Path(d) / ".env.example"
        example.write_text(example_text)
        return check(ops_dir, example)


class TestCheckComposeEnv(unittest.TestCase):
    def test_good_wiring_passes(self):
        self.assertEqual(_check(GOOD), [])

    def test_unknown_var_fails(self):
        bad = GOOD.replace("${PF_JWT_SECRET:-dev-fixed}", "${PF_JWT_SECRET2:-dev-fixed}")
        failures = _check(bad)
        self.assertTrue(any("PF_JWT_SECRET2" in f and "not in .env.example" in f for f in failures))

    def test_default_drift_fails(self):
        bad = GOOD.replace(
            "PF_S3_BUCKET: ${PF_S3_BUCKET:-detag-dev}",
            "PF_S3_BUCKET: ${PF_S3_BUCKET:-other-bucket}",
        )
        failures = _check(bad)
        self.assertTrue(any("PF_S3_BUCKET" in f and "differs" in f for f in failures))

    def test_divergent_endpoint_default_allowed(self):
        # PF_S3_ENDPOINT:-http://minio:9000 differs from the example's
        # http://localhost:9000 by design (container vs host) — the
        # good fixture already relies on this, so it must pass.
        self.assertEqual(_check(GOOD), [])

    def test_literal_password_fails(self):
        bad = GOOD.replace(
            "POSTGRES_PASSWORD: ${PF_POSTGRES_PASSWORD:-pf}",
            "POSTGRES_PASSWORD: hunter2",
        )
        failures = _check(bad)
        self.assertTrue(any("POSTGRES_PASSWORD" in f and "bare literal" in f for f in failures))

    def test_literal_url_password_fails(self):
        bad = GOOD.replace(
            "PF_DATABASE_URL: postgres://${PF_POSTGRES_USER:-pf}:${PF_POSTGRES_PASSWORD:-pf}@postgres:5432/${PF_POSTGRES_DB:-pf}",
            "PF_DATABASE_URL: postgres://pf:hunter2@postgres:5432/pf",
        )
        failures = _check(bad)
        self.assertTrue(any("PF_DATABASE_URL" in f and "userinfo" in f for f in failures))

    def test_shell_escape_to_unset_var_fails(self):
        bad = GOOD.replace("$${POSTGRES_DB:-pf}", "$${POSTGRES_SCHEMA:-pf}")
        failures = _check(bad)
        self.assertTrue(any("POSTGRES_SCHEMA" in f for f in failures))

    def test_real_secret_literal_fails(self):
        # The fake key is assembled at runtime: a literal sk_live_...
        # token in this file would trip the repo's own gitleaks gate.
        fake_key = "sk_" + "live_" + "51H7examplekeyhere"
        bad = GOOD + f"  # note: {fake_key}\n"
        failures = _check(bad)
        self.assertTrue(any("real-secret" in f for f in failures))

    def test_comment_interpolation_ignored(self):
        # A ${VAR} in a comment is prose, not a reference.
        commented = GOOD + "# Env: every var is explicit `environment:` with a `${VAR:-default}`\n"
        self.assertEqual(_check(commented), [])


if __name__ == "__main__":
    unittest.main()
