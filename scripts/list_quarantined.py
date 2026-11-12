#!/usr/bin/env python3
"""List quarantined tests (P0-099).

A quarantined test carries `#[ignore = "flaky:<issue-id>"]` and an
ISSUES.md entry (see the "Flaky-test policy" in
docs/05-QUALITY/01-test-strategy.md). This script scans the Rust
sources, prints one row per quarantined test (test, issue, file, and
whether the ISSUES.md entry exists), and always exits 0 — it is a
listing, not a gate. The nightly job runs it so the quarantined set
is visible every day instead of rotting silently.

Usage: python3 scripts/list_quarantined.py [--root DIR]
       just list-quarantined
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# #[ignore = "flaky:FLAKY-001"] — the tag, then the test fn that follows.
IGNORE_RE = re.compile(r'#\[ignore\s*=\s*"flaky:([^"]+)"\]')
FN_RE = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)", re.MULTILINE)


def find_quarantined(root: Path) -> list[dict[str, str]]:
    """[{test, issue, file}] for every flaky-ignore attribute found."""
    found: list[dict[str, str]] = []
    for path in sorted((root / "crates").rglob("*.rs")) + sorted((root / "apps").rglob("*.rs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        for match in IGNORE_RE.finditer(text):
            fn = FN_RE.search(text, match.end())
            found.append({
                "test": fn.group(1) if fn else "(unknown fn)",
                "issue": match.group(1),
                "file": str(path.relative_to(root)),
            })
    return found


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args(argv)
    root = args.root
    quarantined = find_quarantined(root)
    issues = (root / "docs" / "09-STATE" / "ISSUES.md").read_text(encoding="utf-8", errors="replace")
    if not quarantined:
        print("list_quarantined: no quarantined tests (no #[ignore = \"flaky:...\"] found)")
        return 0
    print(f"{'test':<52}{'issue':<12}{'ISSUES.md':<10}file")
    for item in quarantined:
        has_entry = "yes" if item["issue"] in issues else "NO — add it"
        print(f"{item['test']:<52}{item['issue']:<12}{has_entry:<10}{item['file']}")
    print(f"list_quarantined: {len(quarantined)} quarantined test(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
