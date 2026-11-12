"""Tests for scripts/coverage_gate.py (P0-098).

The gate parses lcov.info and compares per-crate line coverage
against the NFR-50 floors. Fixtures are small synthetic lcov
snippets — no real report needed.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "coverage_gate.py"


def lcov(*records: tuple[str, int, int]) -> str:
    """One lcov record per (path, lines-found, lines-hit)."""
    out: list[str] = []
    for path, found, hit in records:
        out.append(f"SF:{path}")
        out.append(f"LF:{found}")
        out.append(f"LH:{hit}")
        out.append("end_of_record")
    return "\n".join(out) + "\n"


def run(script_args: list[str], lcov_text: str) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as tmp:
        lcov_path = Path(tmp) / "lcov.info"
        lcov_path.write_text(lcov_text, encoding="utf-8")
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--lcov", str(lcov_path), *script_args],
            capture_output=True, text=True, check=False,
        )


class TestCoverageGate(unittest.TestCase):
    def test_all_above_floors_report_mode_exits_zero(self) -> None:
        text = lcov(
            ("/repo/crates/core/src/lib.rs", 100, 90),
            ("/repo/crates/engine/src/lib.rs", 100, 85),
            ("/repo/apps/api/src/main.rs", 100, 80),
            ("/repo/apps/worker/src/main.rs", 100, 75),
        )
        proc = run([], text)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("all crates at or above their floors", proc.stdout)

    def test_below_floor_report_mode_still_exits_zero(self) -> None:
        # Report-only until Phase 2: a miss is printed, not enforced.
        text = lcov(("/repo/crates/core/src/lib.rs", 100, 50))
        proc = run([], text)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("BELOW FLOOR", proc.stdout)
        self.assertIn("report-only", proc.stdout)

    def test_below_floor_gate_mode_exits_one(self) -> None:
        text = lcov(("/repo/crates/core/src/lib.rs", 100, 50))
        proc = run(["--gate"], text)
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("GATE FAILED", proc.stdout)

    def test_test_files_not_counted(self) -> None:
        # Integration tests and benches are instrumented too, but the
        # floors measure production sources only.
        text = lcov(
            ("/repo/crates/core/src/lib.rs", 100, 90),
            ("/repo/crates/core/tests/prop.rs", 100, 0),
            ("/repo/crates/engine/benches/detect.rs", 100, 0),
        )
        proc = run([], text)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("crates/core", proc.stdout)
        # core shows 100 lines (lib only), not 200.
        core_line = [ln for ln in proc.stdout.splitlines() if ln.startswith("crates/core")][0]
        self.assertIn("100", core_line.split()[1])

    def test_missing_lcov_file_exits_one(self) -> None:
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "--lcov", "/nonexistent/lcov.info"],
            capture_output=True, text=True, check=False,
        )
        self.assertEqual(proc.returncode, 1)


if __name__ == "__main__":
    unittest.main()
