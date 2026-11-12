#!/usr/bin/env python3
"""Check .env.example for drift (P0-011, NFR-52).

Compares three sets of PF_* variables and fails on any mismatch:
  1. keys in .env.example,
  2. PF_* vars referenced in Rust code (apps/ + crates/),
  3. PF_* vars in the runbook env table
     (docs/06-DELIVERY/03-deploy-runbook.md, "Environment variables").

Failure modes:
  - a var referenced in code or documented in the runbook is missing from
    .env.example  -> add it (with a safe dev default);
  - a var in .env.example is neither referenced in code nor documented in
    the runbook    -> document it in the runbook or remove it.

Runbook shorthand the parser understands:
  - slash groups: PF_RESULTS_TTL_FREE/PRO/BUSINESS_H expands to
    PF_RESULTS_TTL_FREE_H, PF_RESULTS_TTL_PRO_H, PF_RESULTS_TTL_BUSINESS_H.
  - star patterns (e.g. PF_RATELIMIT_*) are not concrete vars and are skipped.

Usage:
  env_check.py [--example PATH] [--runbook PATH] [--code-dir PATH ...]
Defaults: .env.example, docs/06-DELIVERY/03-deploy-runbook.md, apps crates.
Exit 0 when all three sets agree, 1 on drift.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

VAR_RE = re.compile(r"PF_[A-Z][A-Z0-9_/]*")
# A runbook slash-group: PF_RESULTS_TTL_FREE/PRO/BUSINESS_H
SLASH_GROUP_RE = re.compile(r"^(PF_[A-Z0-9_]*_)([A-Z0-9_]+)/(.+)$")


def parse_example(path: Path) -> set[str]:
    """Keys of KEY=VALUE lines in an .env file (comments/blank lines ignored)."""
    keys: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key = line.split("=", 1)[0].strip()
        if key.startswith("PF_"):
            keys.add(key)
    return keys


def expand_slash_group(token: str) -> set[str]:
    """Expand PF_RESULTS_TTL_FREE/PRO/BUSINESS_H into its three vars."""
    match = SLASH_GROUP_RE.match(token)
    if not match:
        return {token}
    prefix, first, rest = match.groups()
    parts = [first, *rest.split("/")]
    suffix = ""
    last = parts[-1]
    if "_" in last:
        middle, _, suffix = last.rpartition("_")
        parts[-1] = middle
        suffix = "_" + suffix
    return {f"{prefix}{part}{suffix}" for part in parts}


def _tokens_in(text: str, after: int = 0) -> set[str]:
    """PF_* tokens in text, skipping star patterns (PF_RATELIMIT_*)."""
    found: set[str] = set()
    for match in VAR_RE.finditer(text):
        token = match.group(0).rstrip("_/")
        if match.end() < len(text) and text[match.end()] == "*":
            continue  # pattern, not a concrete var
        if "/" in token:
            found |= expand_slash_group(token)
        else:
            found.add(token)
    return found


def code_vars(dirs: list[Path]) -> set[str]:
    """PF_* vars referenced in .rs files under the given dirs."""
    found: set[str] = set()
    for directory in dirs:
        for path in directory.rglob("*.rs"):
            found |= _tokens_in(path.read_text(encoding="utf-8", errors="ignore"))
    return found


def runbook_vars(path: Path) -> set[str]:
    """PF_* vars in the runbook's 'Environment variables' section."""
    text = path.read_text(encoding="utf-8")
    start = text.find("## Environment variables")
    if start < 0:
        raise ValueError(f"no '## Environment variables' section in {path}")
    end = text.find("\n## ", start + 1)
    section = text[start : end if end > 0 else len(text)]
    return _tokens_in(section)


def check(
    example_path: Path, runbook_path: Path, code_dirs: list[Path]
) -> tuple[list[str], dict[str, set[str]]]:
    """Return (errors, sets) for the three-way comparison."""
    example = parse_example(example_path)
    code = code_vars(code_dirs)
    runbook = runbook_vars(runbook_path)

    errors: list[str] = []
    missing = (code | runbook) - example
    if missing:
        errors.append(
            "missing from .env.example: " + ", ".join(sorted(missing))
        )
    undocumented = example - code - runbook
    if undocumented:
        errors.append(
            "in .env.example but neither referenced in code nor in the "
            "runbook: " + ", ".join(sorted(undocumented))
        )
    return errors, {"example": example, "code": code, "runbook": runbook}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--example", default=".env.example")
    parser.add_argument(
        "--runbook", default="docs/06-DELIVERY/03-deploy-runbook.md"
    )
    parser.add_argument("--code-dir", dest="code_dirs", action="append",
                        default=["apps", "crates"])
    args = parser.parse_args(argv)

    errors, sets = check(
        Path(args.example),
        Path(args.runbook),
        [Path(d) for d in args.code_dirs],
    )
    print(
        f"env-check: example={len(sets['example'])} code={len(sets['code'])} "
        f"runbook={len(sets['runbook'])}"
    )
    if errors:
        for error in errors:
            print(f"env-check FAIL: {error}")
        return 1
    print("env-check: OK — .env.example, code and runbook agree")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
