#!/usr/bin/env python3
"""Check compose files' env wiring (P0-089).

Static only: it parses every ops/*.yml, it never starts a
container, so it runs in CI and on this VM (where docker is gone
after the reset — see DOCKER-001).

It fails (exit 1) when any of these is true:

  - a compose interpolation `${VAR}` / `${VAR:-default}` names a
    var that is not a key in .env.example (every var compose
    references must be overridable from .env);
  - a `${VAR:-default}` default differs from the .env.example
    value for VAR, except the documented divergences below
    (a compose default and the example default serve different
    runtimes — container vs host — so they may legitimately
    differ, but only where the divergence is named here);
  - an environment value under a secret-named key (PASSWORD,
    SECRET, TOKEN, PRIVATE, KEY) is a bare literal instead of a
    `${...}` interpolation — or, for a URL template, has literal
    credentials in its userinfo;
  - a `$${VAR}` shell escape (a container-side expansion, not a
    compose interpolation) names a var that is not set in the
    same service's environment;
  - a real-secret literal pattern (sk_live_/sk_test_, AKIA...,
    a PEM block) appears anywhere in a compose file.

Documented divergences (compose default vs .env.example):
  - PF_S3_ENDPOINT: compose defaults to http://minio:9000 (the
    service DNS name — inside a container localhost is the
    container itself, P0-085); .env.example defaults to
    http://localhost:9000 (the host-side default for native runs).

Usage:
  check_compose_env.py [--ops-dir PATH] [--example PATH]
Defaults: ops/ and .env.example (relative to the repo root, the
script's parent's parent). Exit 0 when the wiring is clean, 1 on
any failure.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

import yaml

# Compose interpolation: ${VAR} or ${VAR:-default}. The negative
# lookbehind skips $${VAR} shell escapes (container-side, rule R4).
INTERP_RE = re.compile(r"(?<!\$)\$\{([A-Z_][A-Z0-9_]*)(?::-([^}]*))?\}")
# Container-side expansion inside a command/healthcheck: $${VAR}.
SHELL_ESCAPE_RE = re.compile(r"\$\$\{([A-Z_][A-Z0-9_]*)(?::-([^}]*))?\}")
# A secret-bearing env key: credentials (PASSWORD/SECRET/TOKEN/
# PRIVATE/KEY) or a URL (a DATABASE_URL can hide literal credentials
# in its userinfo — the userinfo check below covers it, while a
# plain service URL with no userinfo passes untouched).
SECRET_KEY_RE = re.compile(r"PASSWORD|SECRET|TOKEN|PRIVATE|KEY|URL")
REAL_SECRET_RE = re.compile(r"sk_(live|test)_[A-Za-z0-9]+|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----")

# Compose defaults that legitimately differ from .env.example
# (container runtime vs host runtime — see the module docstring).
DIVERGENT_DEFAULTS = {
    "PF_S3_ENDPOINT": "http://minio:9000",
}


def parse_example(path: Path) -> dict[str, str]:
    """Keys and values of KEY=VALUE lines in an .env file."""
    values: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip()
    return values


def _env_map(service: dict) -> dict[str, str]:
    env = service.get("environment") or {}
    if isinstance(env, list):
        out: dict[str, str] = {}
        for item in env:
            key, _, value = str(item).partition("=")
            out[key] = value
        return out
    return {str(k): str(v) for k, v in dict(env).items()}


def _userinfo_has_literal(value: str) -> bool:
    """True when a URL template's userinfo holds literal credentials.

    The userinfo (between :// and @) must consist only of
    ${...} interpolations and the ':' separator — any word
    character left after removing the interpolations is a
    literal username or password.
    """
    if "://" not in value or "@" not in value:
        return False
    userinfo = value.split("://", 1)[1].split("@", 1)[0]
    remainder = INTERP_RE.sub("", userinfo)
    return bool(re.search(r"[A-Za-z0-9]", remainder))


def check_file_text(path: Path, example: dict[str, str]) -> list[str]:
    """Rules R1, R2, R5 over one compose file's raw text."""
    failures: list[str] = []
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as e:
        return [f"{path}: cannot read file: {e}"]
    for lineno, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        for match in INTERP_RE.finditer(line):
            var, default = match.group(1), match.group(2)
            if var not in example:
                failures.append(
                    f"{path.name}:{lineno} ${{{var}}} is not in .env.example"
                )
            elif default is not None and default != example[var]:
                if DIVERGENT_DEFAULTS.get(var) != default:
                    failures.append(
                        f"{path.name}:{lineno} ${{{var}:-{default}}} default "
                        f"differs from .env.example ({example[var]!r})"
                    )
    if REAL_SECRET_RE.search(text):
        failures.append(f"{path.name}: contains a real-secret literal pattern")
    return failures


def check_file_services(path: Path) -> list[str]:
    """Rules R3, R4 over one compose file's parsed services."""
    failures: list[str] = []
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, yaml.YAMLError) as e:
        return [f"{path}: cannot parse compose file: {e}"]
    services = (data or {}).get("services") or {}
    if not isinstance(services, dict):
        return [f"{path}: 'services' is not a mapping"]
    for name, service in services.items():
        if not isinstance(service, dict):
            continue
        env = _env_map(service)
        for key, value in env.items():
            if not SECRET_KEY_RE.search(key):
                continue
            value = value.strip()
            if value.startswith("${") and value.endswith("}") and INTERP_RE.fullmatch(value):
                continue  # pure interpolation — the only literal-free form
            if _userinfo_has_literal(value):
                failures.append(
                    f"{path.name}: service {name!r} env {key} has literal "
                    "credentials in its URL userinfo"
                )
            elif "://" not in value:
                failures.append(
                    f"{path.name}: service {name!r} env {key} is a bare "
                    f"literal, not a ${{...}} interpolation (got {value!r})"
                )
        # R4: $${VAR} shell escapes must name a var this service sets.
        text = yaml.dump(service, default_flow_style=False)
        for match in SHELL_ESCAPE_RE.finditer(text):
            var = match.group(1)
            if var not in env:
                failures.append(
                    f"{path.name}: service {name!r} uses $${{{var}}} but "
                    "does not set it in its own environment"
                )
    return failures


def check(ops_dir: Path, example_path: Path) -> list[str]:
    """Check every ops/*.yml against .env.example. Returns failures."""
    try:
        example = parse_example(example_path)
    except OSError as e:
        return [f"{example_path}: cannot read file: {e}"]
    failures: list[str] = []
    for path in sorted(ops_dir.glob("*.yml")):
        failures.extend(check_file_text(path, example))
        failures.extend(check_file_services(path))
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parent.parent
    parser.add_argument("--ops-dir", type=Path, default=root / "ops")
    parser.add_argument("--example", type=Path, default=root / ".env.example")
    args = parser.parse_args(argv)
    failures = check(args.ops_dir, args.example)
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print("OK: compose env wiring is clean (no literal secrets, all vars in .env.example)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
