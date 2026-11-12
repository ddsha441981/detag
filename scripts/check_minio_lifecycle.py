#!/usr/bin/env python3
"""Check the MinIO lifecycle config enforces the retention ceiling (P0-060).

Static only: it parses ops/minio/lifecycle.json, it never talks to
MinIO, so it runs in CI and on this VM (where docker is gone after
the reset — see DOCKER-001). The live check — actually importing
the rules and watching an object expire — waits for CI/E0.9; this
script is the gate until then.

The rule it guards is the retention design in the runbook
("Storage lifecycle & retention"): lifecycle granularity is days,
so the per-plan result TTLs (Free 6 h, Pro 24 h, FR-J-09) cannot be
expressed in ILM at all — the detag-gc job enforces them from
artifacts.expires_at. What ILM does is the ceiling: nothing in the
bucket lives past 72 h (the Business TTL). A rule that expired
sooner would delete results the user paid to keep for 72 h; a rule
that expired later (or a prefix-scoped rule that missed some
objects) would leave user images lying around past their promise.

It fails (exit 1) when any of these is true:

  - the file is missing, not JSON, or not an object with a
    non-empty "Rules" list;
  - a rule has no ID, a duplicate ID, or Status other than
    "Enabled" (a disabled ceiling is no ceiling);
  - a rule has no Expiration.Days, or Days outside 1..3 (0 is not
    a valid expiry; >3 breaks the 72 h promise);
  - a rule's Filter.Prefix is not "" (the ceiling is bucket-wide:
    inputs and outputs alike — the key layout has no per-plan or
    per-kind prefix to scope by, ADR-0011);
  - an AbortIncompleteMultipartUpload rule, when present, has
    DaysAfterInitiation outside 1..7.

Usage:
  check_minio_lifecycle.py [--lifecycle PATH]
Default: ops/minio/lifecycle.json (relative to the repo root, the
script's parent's parent).
Exit 0 when the file passes, 1 on any failure.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# The ceiling: the longest result TTL (Business, 72 h, FR-J-09).
MAX_EXPIRY_DAYS = 3
# Aborted multipart uploads leave orphaned parts; 7 days matches
# R2's built-in default and is the longest sane wait here.
MAX_ABORT_DAYS = 7


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def default_path() -> Path:
    return repo_root() / "ops" / "minio" / "lifecycle.json"


def check(path: Path) -> list[str]:
    """Return a list of violations (empty means the file passes)."""
    errors: list[str] = []
    try:
        data = json.loads(path.read_text())
    except FileNotFoundError:
        return [f"{path}: file not found"]
    except json.JSONDecodeError as exc:
        return [f"{path}: not valid JSON: {exc}"]
    if not isinstance(data, dict):
        return [f"{path}: top level is not an object"]
    rules = data.get("Rules")
    if not isinstance(rules, list) or not rules:
        return [f"{path}: 'Rules' must be a non-empty list"]

    seen_ids: set[str] = set()
    for i, rule in enumerate(rules):
        where = f"{path}: Rules[{i}]"
        if not isinstance(rule, dict):
            errors.append(f"{where}: rule is not an object")
            continue
        rule_id = rule.get("ID")
        if not isinstance(rule_id, str) or not rule_id:
            errors.append(f"{where}: rule has no ID")
        elif rule_id in seen_ids:
            errors.append(f"{where}: duplicate rule ID {rule_id!r}")
        else:
            seen_ids.add(rule_id)
        if rule.get("Status") != "Enabled":
            errors.append(f"{where}: Status must be 'Enabled'")
        prefix = rule.get("Filter", {}).get("Prefix") if isinstance(rule.get("Filter"), dict) else None
        if prefix != "":
            errors.append(
                f"{where}: Filter.Prefix must be '' (bucket-wide ceiling); got {prefix!r}"
            )
        expiration = rule.get("Expiration")
        if not isinstance(expiration, dict) or not isinstance(expiration.get("Days"), int):
            errors.append(f"{where}: Expiration.Days must be an integer number of days")
        elif not 1 <= expiration["Days"] <= MAX_EXPIRY_DAYS:
            errors.append(
                f"{where}: Expiration.Days={expiration['Days']} breaks the "
                f"{MAX_EXPIRY_DAYS}-day (72 h) ceiling"
            )
        abort = rule.get("AbortIncompleteMultipartUpload")
        if abort is not None:
            days = abort.get("DaysAfterInitiation") if isinstance(abort, dict) else None
            if not isinstance(days, int) or not 1 <= days <= MAX_ABORT_DAYS:
                errors.append(
                    f"{where}: AbortIncompleteMultipartUpload.DaysAfterInitiation "
                    f"must be 1..{MAX_ABORT_DAYS}, got {days!r}"
                )
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lifecycle", type=Path, default=default_path())
    args = parser.parse_args(argv)
    errors = check(args.lifecycle)
    for error in errors:
        print(f"FAIL: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"OK: {args.lifecycle} enforces the {MAX_EXPIRY_DAYS}-day retention ceiling")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
