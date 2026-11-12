#!/usr/bin/env python3
"""Check the MinIO dev compose file keeps the bucket private (P0-054).

Static only: it parses ops/compose.minio.yml, it never starts a
container, so it runs in CI and on this VM (where docker is gone
after the reset — see DOCKER-001). The live check — actually
creating the bucket and probing anonymous access — waits for
CI/E0.9; this script is the gate until then.

It fails (exit 1) when any of these is true:

  - the bootstrap service does not explicitly set the bucket's
    anonymous policy to `none` (`mc anonymous set none`), or it
    sets any public policy (`anonymous set public|download|upload`,
    `policy set public*`) anywhere in the file;
  - the bucket name or the root credentials are hardcoded instead
    of coming from PF_S3_* env interpolation;
  - the console address is missing (dev console must exist here)
    or any service publishes a `ports:` mapping (host networking
    only — ADR-0014);
  - the minio service has no named volume for /data (objects are
    durable state) or no healthcheck.

Usage:
  check_minio_policy.py [--compose PATH]
Default: ops/compose.minio.yml (relative to the repo root, the
script's parent's parent).
Exit 0 when the file passes, 1 on any failure.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

import yaml

DEFAULT_BUCKET = "detag-dev"
PUBLIC_POLICY = re.compile(
    r"(anonymous\s+set\s+(public|download|upload)|policy\s+set\s+public)",
    re.IGNORECASE,
)
PRIVATE_POLICY = re.compile(r"anonymous\s+set\s+none\b", re.IGNORECASE)
ENV_REF = re.compile(r"\$\{([A-Z0-9_]+)(?::-([^}]*))?\}")


def load_compose(path: Path) -> dict:
    with path.open() as f:
        data = yaml.safe_load(f)
    if not isinstance(data, dict):
        raise ValueError(f"{path}: top level is not a mapping")
    return data


def _command_text(service: dict) -> str:
    command = service.get("command", "")
    if isinstance(command, list):
        return "\n".join(str(c) for c in command)
    return str(command)


def _env_text(service: dict) -> dict[str, str]:
    env = service.get("environment", {})
    if isinstance(env, list):
        out: dict[str, str] = {}
        for item in env:
            key, _, value = str(item).partition("=")
            out[key] = value
        return out
    return {str(k): str(v) for k, v in dict(env).items()}


def check(path: Path) -> list[str]:
    """Return a list of failure messages (empty when the file passes)."""
    failures: list[str] = []
    try:
        data = load_compose(path)
    except (OSError, ValueError, yaml.YAMLError) as e:
        return [f"{path}: cannot parse compose file: {e}"]

    text = path.read_text()
    services = data.get("services", {})
    if not isinstance(services, dict):
        return [f"{path}: 'services' is not a mapping"]

    minio = services.get("minio")
    bootstrap = services.get("bootstrap")
    if not isinstance(minio, dict):
        failures.append("missing 'minio' service")
    if not isinstance(bootstrap, dict):
        failures.append("missing 'bootstrap' service")
    if failures:
        return failures

    # 1. No public policy anywhere in the file, and the bootstrap
    #    explicitly locks the bucket to anonymous-none.
    if PUBLIC_POLICY.search(text):
        failures.append("file sets a public anonymous/policy (public/download/upload)")
    bootstrap_cmd = _command_text(bootstrap)
    if not PRIVATE_POLICY.search(bootstrap_cmd):
        failures.append(
            "bootstrap does not run 'mc anonymous set none' on the bucket"
        )
    if "mc mb " not in bootstrap_cmd and "mc mb\n" not in bootstrap_cmd:
        failures.append("bootstrap does not create the bucket ('mc mb')")

    # 2. Bucket + credentials come from PF_S3_* env, never hardcoded.
    #    Every storage env value must be exactly one ${...}
    #    interpolation — a bare literal (even the right value) is a
    #    failure, because the file must stay overridable from .env.
    #    (The bootstrap command's $${PF_S3_BUCKET} is a shell
    #    expansion of the container env var, not a compose
    #    interpolation — it is checked via the env lines below.)
    bootstrap_env = _env_text(bootstrap)
    for var in (
        "PF_S3_ENDPOINT",
        "PF_S3_BUCKET",
        "PF_S3_ACCESS_KEY",
        "PF_S3_SECRET_KEY",
    ):
        value = bootstrap_env.get(var, "")
        if not ENV_REF.fullmatch(value.strip()):
            failures.append(
                f"bootstrap env {var} is not a ${{...}} interpolation (got {value!r})"
            )
    minio_env = _env_text(minio)
    for var, want in (
        ("MINIO_ROOT_USER", "PF_S3_ACCESS_KEY"),
        ("MINIO_ROOT_PASSWORD", "PF_S3_SECRET_KEY"),
    ):
        value = minio_env.get(var, "")
        if not ENV_REF.fullmatch(value.strip()) or want not in value:
            failures.append(f"{var} does not come from ${{{want}:-...}} (got {value!r})")
    if "minioadmin" in text:
        # Only allowed as the ${...:-minioadmin} dev default in an
        # environment value — never as a bare literal (and the
        # header comment mentions it only in prose, so check env
        # values, not the raw file).
        for name, service in services.items():
            if not isinstance(service, dict):
                continue
            for key, value in _env_text(service).items():
                if "minioadmin" in value and not ENV_REF.fullmatch(value.strip()):
                    failures.append(
                        f"service {name!r} env {key} has a bare 'minioadmin' literal"
                    )

    # 3. Console is dev-only: present here, and nothing publishes ports.
    minio_cmd = _command_text(minio)
    if "--console-address" not in minio_cmd or ":9001" not in minio_cmd:
        failures.append("minio does not expose the dev console on :9001")
    for name, service in services.items():
        if isinstance(service, dict) and "ports" in service:
            failures.append(f"service {name!r} publishes ports: (host networking only, ADR-0014)")

    # 4. Durable data + healthcheck.
    volumes = minio.get("volumes") or []
    if not any(str(v).endswith(":/data") for v in volumes):
        failures.append("minio has no volume mounted on /data")
    if "healthcheck" not in minio:
        failures.append("minio has no healthcheck")
    if bootstrap.get("restart") != "no":
        failures.append("bootstrap must be one-shot (restart: \"no\")")

    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--compose",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "ops" / "compose.minio.yml",
        help="path to the MinIO compose file",
    )
    args = parser.parse_args(argv)
    failures = check(args.compose)
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print(f"OK: {args.compose} keeps the bucket private")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
