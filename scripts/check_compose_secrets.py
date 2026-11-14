#!/usr/bin/env python3
"""No literal secrets in compose files (P0-105).

Secret policy (CONTRIBUTING.md, "Secrets policy"): all secrets live
in env vars, never in committed files. Compose is the one place a
literal secret can hide in plain sight — a `${VAR:-default}` fallback
is a literal value, and the shared base file layers under prod too,
so a dev default there is also prod's default.

Rules, enforced here:
- `ops/compose.base.yml` + `ops/compose.prod.yml` (strict — prod
  layers over them): a secret-named var may only be a BARE
  `${VAR}` reference. Any literal (a plain value, or a `:-default`)
  fails.
- The dev-only files (`compose.dev.yml`, `compose.minio.yml`,
  `compose.db.yml`, `compose.redis.yml`): a secret-named var may be
  a bare reference, or one of the documented dev placeholders
  (`pf`, `minioadmin`, `dev-fixed`, `minioadmin-secret-key` — the
  `.env.example` values). Any other literal fails.
- A compose file not in either list is treated as strict (fail
  closed).

Secret-named means the key matches SECRET | PASSWORD | TOKEN |
PRIVATE_KEY (case-insensitive). Non-secret vars (users, DB names,
ports, hosts) keep their defaults — they are identifiers, not
credentials.

The dev values themselves come from the env file (`--env-file .env`,
copied from `.env.example`); the compose files never carry them in
the strict files. `just compose-secrets-check` runs this; CI runs
that recipe in the verify job.

Usage: python3 scripts/check_compose_secrets.py [--root DIR]
       just compose-secrets-check
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# A secret-named env key: SECRET, PASSWORD, TOKEN, PRIVATE_KEY.
SECRET_KEY_RE = re.compile(r"(SECRET|PASSWORD|TOKEN|PRIVATE_KEY)", re.IGNORECASE)
# `KEY: VALUE` — an environment entry in a compose file.
ENTRY_RE = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(\S.*?)\s*$")
# A bare reference: ${VAR} (optionally quoted).
BARE_REF_RE = re.compile(r"^['\"]?\$\{[A-Za-z_][A-Za-z0-9_]*\}['\"]?$")
# A reference with a literal default: ${VAR:-default} / ${VAR-default}.
DEFAULT_REF_RE = re.compile(
    r"^['\"]?\$\{[A-Za-z_][A-Za-z0-9_]*:-?([^}]*)\}['\"]?$"
)

# Files prod layers over: no literal secret value at all.
STRICT_FILES = {"compose.base.yml", "compose.prod.yml"}
# Dev-only files: the documented placeholders are allowed.
DEV_FILES = {
    "compose.dev.yml",
    "compose.minio.yml",
    "compose.db.yml",
    "compose.redis.yml",
}
# The only literal secret values allowed anywhere (dev-only files):
# the `.env.example` placeholders. Anything else is a real-looking
# secret and fails.
# NOTE: `minioadmin-secret-key` is here because Garage refuses secret
# keys shorter than 16 chars on key import (CI-FAIL-017) — the plain
# `minioadmin` placeholder (10 chars) kills the smoke stack. Do NOT
# "simplify" this back to `minioadmin`; the value must stay >= 16
# chars (see the PF_S3_SECRET_KEY comment in .env.example).
DEV_PLACEHOLDERS = {"pf", "minioadmin", "dev-fixed", "minioadmin-secret-key"}


def classify_value(value: str) -> tuple[str, str | None]:
    """("bare" | "placeholder" | "literal", default-or-value).

    "bare" is a `${VAR}` reference; "placeholder" is a literal in
    DEV_PLACEHOLDERS (or a `${VAR:-placeholder}` default);
    "literal" is anything else.
    """
    value = value.strip().strip("'\"").strip()
    if BARE_REF_RE.match(value):
        return "bare", None
    match = DEFAULT_REF_RE.match(value)
    if match:
        default = match.group(1)
        if default in DEV_PLACEHOLDERS:
            return "placeholder", default
        return "literal", default
    if value in DEV_PLACEHOLDERS:
        return "placeholder", value
    return "literal", value


def check_file(path: Path) -> list[str]:
    """Violation messages for one compose file (empty when clean)."""
    strict = path.name in STRICT_FILES or path.name not in DEV_FILES
    violations: list[str] = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        match = ENTRY_RE.match(line)
        if not match:
            continue
        key, value = match.group(1), match.group(2)
        if not SECRET_KEY_RE.search(key):
            continue
        kind, default = classify_value(value)
        if kind == "bare":
            continue
        if kind == "placeholder" and not strict:
            continue
        violations.append(
            f"{path.name}:{lineno}: {key} has a literal secret value"
            + (f" (default {default!r})" if default is not None else "")
            + (" — strict file: use a bare ${VAR} reference" if strict else "")
        )
    return violations


def check_root(root: Path) -> list[str]:
    """Violation messages for every ops/compose.*.yml under root."""
    violations: list[str] = []
    ops = root / "ops"
    if not ops.is_dir():
        return ["ops/ directory not found"]
    for path in sorted(ops.glob("compose.*.yml")):
        violations.extend(check_file(path))
    return violations


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repo root (default: parent of scripts/)",
    )
    args = parser.parse_args(argv)
    violations = check_root(args.root)
    if violations:
        print("compose secrets check FAILED — literal secret values:", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print("compose secrets check OK: no literal secret values in ops/compose.*.yml")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
