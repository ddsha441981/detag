#!/usr/bin/env python3
"""Run one test repeatedly to catch (or clear) a flake (P0-099).

A flaky test passes in isolation and fails under load, so a single
green run proves nothing. This helper runs the named test N times
in fresh processes and reports how many passed:

    just test-repeat sigterm_drains_in_flight_request_and_exits_zero 5

The name is cargo's substring filter (the same filter `cargo test
<name>` uses), so a short unique fragment is enough. Exit 0 only if
every run passed; otherwise exit 1 with the failing run's output
tail. Each run is a separate `cargo test` invocation — the test
binary is rebuilt once and reused, but the process (and its timing)
is fresh every time, which is the point.

NOTE on the name: the ticket spec wrote `just test:repeat`, but just
does not allow `:` in a recipe name (it is the recipe-body separator)
— `test-repeat` is the same helper in the repo's dash convention
(`fixtures-check`, `coverage-gate`, ...).

Usage: python3 scripts/test_repeat.py <name> <n> [--package PKG]
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import time

RESULT_RE = re.compile(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed")


def run_once(name: str, package: str | None, env: dict[str, str]) -> tuple[bool, str, float]:
    """One `cargo test` invocation; returns (passed, output, seconds)."""
    cmd = ["cargo", "test"]
    if package:
        cmd += ["-p", package]
    cmd += ["--", name]
    start = time.monotonic()
    proc = subprocess.run(cmd, capture_output=True, text=True, env=env, check=False)
    elapsed = time.monotonic() - start
    output = proc.stdout + proc.stderr
    return proc.returncode == 0, output, elapsed


def summarize(results: list[tuple[bool, str, float]]) -> str:
    """One-line summary: `3/5 passed` (+ the failing runs' numbers)."""
    passed = sum(1 for ok, _, _ in results if ok)
    failed_runs = [i + 1 for i, (ok, _, _) in enumerate(results) if not ok]
    line = f"{passed}/{len(results)} passed"
    if failed_runs:
        line += f" (failed runs: {', '.join(map(str, failed_runs))})"
    return line


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("name", help="test name (cargo substring filter)")
    parser.add_argument("n", type=int, help="how many times to run it")
    parser.add_argument("--package", "-p", default=None,
                        help="cargo package (default: whole workspace)")
    args = parser.parse_args(argv)
    if args.n < 1:
        print("test_repeat: n must be >= 1", file=sys.stderr)
        return 2
    env = dict(os.environ)
    env.setdefault("SQLX_OFFLINE", "true")
    results: list[tuple[bool, str, float]] = []
    for i in range(1, args.n + 1):
        ok, output, elapsed = run_once(args.name, args.package, env)
        results.append((ok, output, elapsed))
        print(f"run {i}/{args.n}: {'PASS' if ok else 'FAIL'} ({elapsed:.1f}s)")
        if not ok:
            tail = "\n".join(output.strip().splitlines()[-15:])
            print(f"--- output tail (run {i}) ---\n{tail}\n--- end ---")
    print(f"test_repeat: {summarize(results)}")
    return 0 if all(ok for ok, _, _ in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
