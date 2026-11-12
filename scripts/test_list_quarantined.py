"""Tests for scripts/list_quarantined.py (P0-099)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import list_quarantined  # noqa: E402


class TestFindQuarantined(unittest.TestCase):
    def _root(self, files: dict[str, str]) -> Path:
        import tempfile
        tmp = Path(tempfile.mkdtemp())
        for rel, text in files.items():
            path = tmp / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return tmp

    def test_finds_tagged_test(self) -> None:
        root = self._root({
            "crates/core/tests/foo.rs": (
                '#[ignore = "flaky:FLAKY-001"]\n'
                "#[test]\n"
                "fn sigterm_drains_in_flight_request_and_exits_zero() {}\n"
            ),
        })
        found = list_quarantined.find_quarantined(root)
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0]["test"], "sigterm_drains_in_flight_request_and_exits_zero")
        self.assertEqual(found[0]["issue"], "FLAKY-001")
        self.assertEqual(found[0]["file"], "crates/core/tests/foo.rs")

    def test_ignores_plain_ignore_and_untagged(self) -> None:
        root = self._root({
            "crates/core/tests/foo.rs": (
                '#[ignore]\n#[test]\nfn plain_ignored() {}\n\n'
                '#[test]\nfn normal() {}\n'
            ),
        })
        self.assertEqual(list_quarantined.find_quarantined(root), [])

    def test_empty_tree(self) -> None:
        root = self._root({"crates/core/src/lib.rs": "pub fn x() {}\n"})
        self.assertEqual(list_quarantined.find_quarantined(root), [])


if __name__ == "__main__":
    unittest.main()
