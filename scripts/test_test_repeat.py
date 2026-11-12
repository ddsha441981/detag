"""Tests for scripts/test_repeat.py (P0-099).

The counting/summary logic is tested directly; the real `cargo test`
loop is exercised once with a name that matches nothing (fast — 0
tests run, exit 0).
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_repeat  # noqa: E402


class TestSummarize(unittest.TestCase):
    def test_all_pass(self) -> None:
        self.assertEqual(test_repeat.summarize([(True, "", 1.0), (True, "", 1.0)]), "2/2 passed")

    def test_some_fail_lists_runs(self) -> None:
        self.assertEqual(
            test_repeat.summarize([(True, "", 1.0), (False, "", 1.0), (False, "", 1.0)]),
            "1/3 passed (failed runs: 2, 3)",
        )

    def test_all_fail(self) -> None:
        self.assertEqual(
            test_repeat.summarize([(False, "", 1.0)]), "0/1 passed (failed runs: 1)"
        )


class TestCli(unittest.TestCase):
    def test_n_below_one_exits_two(self) -> None:
        proc = subprocess.run(
            [sys.executable, str(Path(__file__).resolve().parent / "test_repeat.py"),
             "some_test", "0"],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(proc.returncode, 2)

    def test_run_once_no_match_passes(self) -> None:
        # A name that matches no test: cargo test runs 0 tests, exit 0.
        # (Also proves the script invokes cargo correctly end to end.)
        env = dict(os.environ)
        env.setdefault("SQLX_OFFLINE", "true")
        # cargo lives in ~/.cargo/bin, which is not on PATH when the
        # test suite runs without the usual shell export.
        env["PATH"] = os.path.expanduser("~/.cargo/bin") + ":" + env.get("PATH", "")
        with tempfile.TemporaryDirectory() as tmp:
            cwd = os.getcwd()
            try:
                os.chdir(Path(__file__).resolve().parent.parent)
                ok, output, elapsed = test_repeat.run_once(
                    "no_such_test_xyz_12345", None, env)
            finally:
                os.chdir(cwd)
        self.assertTrue(ok, output[-500:])
        self.assertGreaterEqual(elapsed, 0.0)


if __name__ == "__main__":
    unittest.main()
