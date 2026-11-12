#!/usr/bin/env python3
"""Copy criterion `main`-baseline estimates into the committed baselines.

After `cargo bench -p detag-engine -- --save-baseline main`, criterion
stores per-bench estimates under `target/criterion/<group>/<bench>/main/`
(or `<bench>/main/` for ungrouped benches). This script collects each
`estimates.json` there and writes one summary per bench binary into
`crates/engine/benches/baselines/<bench>.json` (mean/median point
estimates, nanoseconds) — the committed record the next run compares
against. Raw samples stay in `target/` (gitignored) and are never
committed.

Usage: python3 scripts/bench_baseline.py [--criterion-dir DIR]
Defaults: <repo>/target/criterion, <repo>/crates/engine/benches/baselines.
Exit 0 on success, 1 if a bench binary produced no baseline.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

BENCHES = ("detect", "inpaint", "encode", "full_job")


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def collect(criterion_dir: Path) -> dict[str, dict[str, dict[str, float]]]:
    """{bench binary: {bench id: {mean_ns, median_ns}}} from main baselines."""
    found: dict[str, dict[str, dict[str, float]]] = {name: {} for name in BENCHES}
    for estimates in sorted(criterion_dir.rglob("estimates.json")):
        parts = estimates.relative_to(criterion_dir).parts
        # Layout: <bench-binary>/<group>/<bench>/main/estimates.json
        # (group may be absent for ungrouped benches).
        if len(parts) < 3 or parts[-2] != "main":
            continue
        binary = parts[0]
        if binary not in found:
            continue
        bench_id = "/".join(parts[1:-2])
        data = json.loads(estimates.read_text(encoding="utf-8"))
        found[binary][bench_id] = {
            "mean_ns": data["mean"]["point_estimate"],
            "median_ns": data["median"]["point_estimate"],
        }
    return found


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--criterion-dir", type=Path, default=None)
    args = parser.parse_args(argv)
    root = repo_root()
    criterion_dir = args.criterion_dir or (root / "target" / "criterion")
    out_dir = root / "crates" / "engine" / "benches" / "baselines"
    if not criterion_dir.is_dir():
        print(f"bench_baseline: no criterion output at {criterion_dir} — run just bench first",
              file=sys.stderr)
        return 1
    found = collect(criterion_dir)
    missing = [name for name in BENCHES if not found[name]]
    if missing:
        print(f"bench_baseline: no main baseline for: {', '.join(missing)}", file=sys.stderr)
        return 1
    out_dir.mkdir(parents=True, exist_ok=True)
    for name in BENCHES:
        path = out_dir / f"{name}.json"
        doc = {
            "bench": name,
            "baseline": "main",
            "unit": "ns",
            "estimates": dict(sorted(found[name].items())),
        }
        path.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"bench_baseline: wrote {path.relative_to(root)} ({len(found[name])} benches)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
