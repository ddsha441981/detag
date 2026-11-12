#!/usr/bin/env python3
"""Per-crate coverage report + gate for the NFR-50 floors.

Reads the lcov.info produced by `just coverage` (or CI's llvm-cov
step), aggregates line coverage per crate from production sources
only (`crates/<name>/src/`, `apps/<name>/src/` — test/bench/example
files are excluded; they are not the code the floors measure), and
prints one row per crate against its floor:

    crates/core    >= 85%    apps/api    >= 75%
    crates/engine  >= 80%    apps/worker >= 70%

Floors: docs/05-QUALITY/01-test-strategy.md ("Coverage floors").

Modes:
  report (default): print the table, exit 0 always — this is the
    Phase 0/1 mode. Coverage is reported, not enforced.
  --gate: exit 1 if any crate is below its floor. OFF until Phase 2
    (per the P0-098 spec: "report-only until Phase 2, then fails
    below floors"). The Phase 2 ticket flips CI to `just coverage-gate`.

Usage:
  python3 scripts/coverage_gate.py [--lcov PATH] [--gate]
  just coverage        # report mode (runs llvm-cov, then this script)
  just coverage-gate   # gate mode
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# crate path fragment -> (label, floor %)
FLOORS: dict[str, tuple[str, float]] = {
    "crates/core/": ("crates/core", 85.0),
    "crates/engine/": ("crates/engine", 80.0),
    "apps/api/": ("apps/api", 75.0),
    "apps/worker/": ("apps/worker", 70.0),
}


def crate_of(path: str) -> str | None:
    """Crate key for a source file, or None if not counted (tests, benches, etc.)."""
    for fragment in FLOORS:
        if f"{fragment}src/" in path:
            return fragment
    return None


def aggregate(lcov_path: Path) -> dict[str, tuple[int, int]]:
    """{crate fragment: (lines found, lines hit)} from an lcov.info file."""
    totals: dict[str, list[int]] = {key: [0, 0] for key in FLOORS}
    current: str | None = None
    found = hit = 0
    for line in lcov_path.read_text(encoding="utf-8").splitlines():
        if line.startswith("SF:"):
            current = crate_of(line[3:])
            found = hit = 0
        elif line.startswith("LF:") and current is not None:
            found = int(line[3:])
        elif line.startswith("LH:") and current is not None:
            hit = int(line[3:])
        elif line == "end_of_record" and current is not None:
            totals[current][0] += found
            totals[current][1] += hit
            current = None
    return {key: (v[0], v[1]) for key, v in totals.items()}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lcov", type=Path, default=Path("target/llvm-cov/lcov.info"))
    parser.add_argument("--gate", action="store_true",
                        help="exit 1 if any crate is below its floor (Phase 2 mode)")
    args = parser.parse_args(argv)
    if not args.lcov.is_file():
        print(f"coverage_gate: no report at {args.lcov} — run `just coverage` first",
              file=sys.stderr)
        return 1
    totals = aggregate(args.lcov)
    print(f"{'crate':<16}{'lines':>8}{'hit':>8}{'cov %':>8}{'floor':>8}  verdict")
    failures: list[str] = []
    for fragment, (label, floor) in FLOORS.items():
        found, hit = totals[fragment]
        pct = 100.0 * hit / found if found else 0.0
        verdict = "PASS" if pct >= floor else "BELOW FLOOR"
        if pct < floor:
            failures.append(label)
        print(f"{label:<16}{found:>8}{hit:>8}{pct:>7.1f}%{floor:>7.0f}%  {verdict}")
    if failures:
        print(f"coverage_gate: below floor: {', '.join(failures)} "
              f"({'GATE FAILED' if args.gate else 'report-only: not enforced until Phase 2'})")
        return 1 if args.gate else 0
    print("coverage_gate: all crates at or above their floors")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
