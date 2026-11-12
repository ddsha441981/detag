#!/usr/bin/env python3
"""Tests for scripts/check_minio_lifecycle.py (P0-060).

Each test writes a lifecycle JSON to a temp file and asserts the
checker accepts the shipped config and rejects each regression
the checker exists to catch.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_minio_lifecycle import check, default_path

GOOD = {
    "Rules": [
        {
            "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 7},
            "Expiration": {"Days": 3},
            "Filter": {"Prefix": ""},
            "ID": "detag-results-ceiling-72h",
            "Status": "Enabled",
        }
    ]
}


def write_config(data) -> Path:
    tmp = tempfile.NamedTemporaryFile(
        "w", suffix=".json", delete=False, encoding="utf-8"
    )
    if isinstance(data, str):
        tmp.write(data)
    else:
        json.dump(data, tmp)
    tmp.close()
    return Path(tmp.name)


def mutate(**overrides):
    import copy

    rule = copy.deepcopy(GOOD["Rules"][0])
    rule.update(overrides)
    return {"Rules": [rule]}


class CheckLifecycleTests(unittest.TestCase):
    def test_shipped_config_passes(self):
        self.assertEqual(check(default_path()), [])

    def test_missing_file(self):
        errors = check(Path("/nonexistent/lifecycle.json"))
        self.assertEqual(len(errors), 1)
        self.assertIn("not found", errors[0])

    def test_invalid_json(self):
        self.assertTrue(check(write_config("{not json")))

    def test_empty_rules(self):
        self.assertTrue(check(write_config({"Rules": []})))

    def test_disabled_rule(self):
        self.assertTrue(check(write_config(mutate(Status="Disabled"))))

    def test_expiry_over_ceiling(self):
        # 4 days breaks the 72 h (Business) promise.
        self.assertTrue(check(write_config(mutate(Expiration={"Days": 4}))))

    def test_expiry_zero_days(self):
        self.assertTrue(check(write_config(mutate(Expiration={"Days": 0}))))

    def test_missing_expiration(self):
        config = mutate()
        del config["Rules"][0]["Expiration"]
        self.assertTrue(check(write_config(config)))

    def test_prefix_scoped_rule(self):
        # A scoped rule would leave objects outside the ceiling.
        self.assertTrue(
            check(write_config(mutate(Filter={"Prefix": "prod/"})))
        )

    def test_duplicate_ids(self):
        config = {"Rules": [GOOD["Rules"][0], GOOD["Rules"][0]]}
        self.assertTrue(check(write_config(config)))

    def test_missing_id(self):
        config = mutate()
        del config["Rules"][0]["ID"]
        self.assertTrue(check(write_config(config)))

    def test_abort_days_over_limit(self):
        self.assertTrue(
            check(
                write_config(
                    mutate(AbortIncompleteMultipartUpload={"DaysAfterInitiation": 30})
                )
            )
        )


if __name__ == "__main__":
    unittest.main()
