"""Unit tests for scripts/check_image_size.py (P0-083, NFR-63).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

import io
import subprocess
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_image_size


class TestSizeOk(unittest.TestCase):
    def test_under_limit_passes(self):
        self.assertTrue(check_image_size.size_ok(149 * check_image_size.MB, 150))
        self.assertTrue(check_image_size.size_ok(150 * check_image_size.MB - 1, 150))

    def test_at_or_over_limit_fails(self):
        self.assertFalse(check_image_size.size_ok(150 * check_image_size.MB, 150))
        self.assertFalse(check_image_size.size_ok(151 * check_image_size.MB, 150))


class TestParseInspectSize(unittest.TestCase):
    def test_plain_integer(self):
        self.assertEqual(check_image_size.parse_inspect_size("123456789\n"), 123456789)

    def test_json_array(self):
        self.assertEqual(
            check_image_size.parse_inspect_size('[{"Id": "sha256:abc", "Size": 98765}]'),
            98765,
        )

    def test_garbage_rejected(self):
        for bad in ("", "not-a-number", "[]", '[{"Id": "x"}]', "{}"):
            with self.assertRaises((ValueError, TypeError), msg=repr(bad)):
                check_image_size.parse_inspect_size(bad)


class TestMain(unittest.TestCase):
    def test_ok_when_under_limit(self):
        with mock.patch.object(
            check_image_size, "inspect_size", return_value=100 * check_image_size.MB
        ):
            out = io.StringIO()
            with redirect_stdout(out):
                rc = check_image_size.main(["--image", "detag-api:check", "--max-mb", "150"])
        self.assertEqual(rc, 0)
        self.assertIn("100.0 MB (< 150 MB)", out.getvalue())

    def test_fails_when_at_limit(self):
        with mock.patch.object(
            check_image_size, "inspect_size", return_value=150 * check_image_size.MB
        ):
            err = io.StringIO()
            with redirect_stderr(err):
                rc = check_image_size.main(["--image", "detag-api:check", "--max-mb", "150"])
        self.assertEqual(rc, 1)
        self.assertIn("NFR-63", err.getvalue())


class TestInspectSize(unittest.TestCase):
    def test_exits_2_when_docker_missing(self):
        with mock.patch.object(
            subprocess, "run", side_effect=FileNotFoundError("docker")
        ):
            with self.assertRaises(SystemExit) as ctx:
                check_image_size.inspect_size("detag-api:check")
        self.assertEqual(ctx.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
