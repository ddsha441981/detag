"""Unit tests for scripts/check_minio_policy.py (P0-054).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

import tempfile
import unittest
from pathlib import Path

from check_minio_policy import check

GOOD = """\
name: detag-minio

services:
  minio:
    image: minio/minio:latest
    container_name: detag-minio
    network_mode: host
    command: ["server", "/data", "--console-address", ":9001"]
    environment:
      MINIO_ROOT_USER: ${PF_S3_ACCESS_KEY:-minioadmin}
      MINIO_ROOT_PASSWORD: ${PF_S3_SECRET_KEY:-minioadmin}
    volumes:
      - miniadata:/data
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:9000/minio/health/live"]
  bootstrap:
    image: minio/mc:latest
    container_name: detag-minio-bootstrap
    network_mode: host
    depends_on:
      minio:
        condition: service_healthy
    restart: "no"
    environment:
      PF_S3_ENDPOINT: ${PF_S3_ENDPOINT:-http://localhost:9000}
      PF_S3_BUCKET: ${PF_S3_BUCKET:-detag-dev}
      PF_S3_ACCESS_KEY: ${PF_S3_ACCESS_KEY:-minioadmin}
      PF_S3_SECRET_KEY: ${PF_S3_SECRET_KEY:-minioadmin}
    entrypoint: ["sh", "-c"]
    command:
      - |
        set -eu
        mc alias set detag "$${PF_S3_ENDPOINT}" "$${PF_S3_ACCESS_KEY}" "$${PF_S3_SECRET_KEY}"
        mc mb --ignore-existing "detag/$${PF_S3_BUCKET}"
        mc anonymous set none "detag/$${PF_S3_BUCKET}"

volumes:
  miniadata:
    name: detag_miniodata
"""


def _check(text: str) -> list[str]:
    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / "compose.minio.yml"
        path.write_text(text)
        return check(path)


class TestCheckMinioPolicy(unittest.TestCase):
    def test_good_file_passes(self):
        self.assertEqual(_check(GOOD), [])

    def test_public_policy_fails(self):
        bad = GOOD.replace(
            "mc anonymous set none", "mc anonymous set public"
        )
        failures = _check(bad)
        self.assertTrue(
            any("public" in f for f in failures), failures
        )

    def test_missing_private_policy_fails(self):
        bad = GOOD.replace(
            '        mc anonymous set none "detag/$${PF_S3_BUCKET}"\n', ""
        )
        failures = _check(bad)
        self.assertTrue(
            any("anonymous set none" in f for f in failures), failures
        )

    def test_hardcoded_bucket_fails(self):
        bad = GOOD.replace("${PF_S3_BUCKET:-detag-dev}", "detag-dev")
        failures = _check(bad)
        self.assertTrue(
            any("PF_S3_BUCKET" in f for f in failures), failures
        )

    def test_bare_root_password_fails(self):
        bad = GOOD.replace(
            "MINIO_ROOT_PASSWORD: ${PF_S3_SECRET_KEY:-minioadmin}",
            "MINIO_ROOT_PASSWORD: minioadmin",
        )
        failures = _check(bad)
        self.assertTrue(
            any("MINIO_ROOT_PASSWORD" in f for f in failures), failures
        )

    def test_ports_mapping_fails(self):
        bad = GOOD.replace(
            "    network_mode: host\n    command: [\"server\"",
            "    network_mode: host\n    ports:\n      - \"9000:9000\"\n    command: [\"server\"",
            1,
        )
        failures = _check(bad)
        self.assertTrue(
            any("ports" in f for f in failures), failures
        )

    def test_missing_volume_fails(self):
        bad = GOOD.replace("      - miniadata:/data\n", "")
        failures = _check(bad)
        self.assertTrue(
            any("/data" in f for f in failures), failures
        )

    def test_missing_console_fails(self):
        bad = GOOD.replace(' "--console-address", ":9001"', "")
        failures = _check(bad)
        self.assertTrue(
            any("console" in f for f in failures), failures
        )


if __name__ == "__main__":
    unittest.main()
