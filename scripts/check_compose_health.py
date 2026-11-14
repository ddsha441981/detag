#!/usr/bin/env python3
"""Check every compose service has a healthcheck (P0-088).

Static only: it renders ops/compose.base.yml layered with the dev
and prod overrides (`docker compose config` — the plugin is
required, same as the other compose checks), it never starts a
container, so it runs in CI and on this VM (where docker is gone
after the reset — see DOCKER-001). The live check — `up --wait`
actually going healthy — waits for CI/E0.9's smoke (P0-090); this
script is the gate until then.

It fails (exit 1) when any of these is true:

  - a service other than the one-shot `bootstrap-lifecycle` has no
    healthcheck. The api/worker healthchecks live in the images
    (Dockerfile.api / Dockerfile.worker HEALTHCHECK), which
    `docker compose config` cannot see until the image is built —
    those two are verified from the Dockerfiles instead;
  - a healthcheck's worst case to healthy (start_period +
    interval * retries) exceeds the 120 s budget `just smoke`
    (P0-090) waits with `--wait-timeout 120`;
  - the api or worker does not wait for postgres/redis/garage
    `service_healthy` and for the bootstrap-lifecycle
    `service_completed_successfully` (the bucket must exist
    before the api serves or the worker consumes jobs), or
    (prod) caddy does not wait for the api `service_healthy`;
  - the bootstrap-lifecycle is not one-shot (`restart: "no"`).

Usage:
  check_compose_health.py [--base PATH] [--dev PATH] [--prod PATH]
      [--api-dockerfile PATH] [--worker-dockerfile PATH]
Defaults are the repo's ops/ files (relative to the repo root, the
script's parent's parent). Exit 0 when both renders pass, 1 on any
failure.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

import yaml

# `just smoke` (P0-090) waits this long for the stack to go healthy:
# `docker compose up -d --wait --wait-timeout 120`. Every
# healthcheck must be able to go green inside that budget.
BUDGET_SECS = 120

# One-shot services have no healthcheck by design — their
# completion is what `depends_on: service_completed_successfully`
# waits on. They must stay one-shot (restart "no").
ONE_SHOT = {"bootstrap-lifecycle"}

# Services whose healthcheck lives in the image, not the compose
# file: `docker compose config` cannot see it until the image is
# built, so the Dockerfile is the source of truth here.
IMAGE_HEALTHCHECK = {"api": "Dockerfile.api", "worker": "Dockerfile.worker"}

# What the api/worker must wait on before starting.
DATA_DEPS = ("postgres", "redis", "garage")

_DURATION = re.compile(r"(\d+(?:\.\d+)?)(ms|s|m|h)")
_HEALTHCHECK_FLAG = re.compile(r"--(interval|timeout|start-period|retries)=(\S+)")


def parse_duration(text: str) -> float:
    """Parse a compose duration (\"5s\", \"10s\", \"1m30s\", \"500ms\") to seconds."""
    total = 0.0
    for amount, unit in _DURATION.findall(str(text)):
        value = float(amount)
        total += {"ms": value / 1000, "s": value, "m": value * 60, "h": value * 3600}[unit]
    return total


def worst_case_seconds(healthcheck: dict) -> float:
    """Worst case for a healthcheck to report healthy.

    start_period (no checks run) + one interval per allowed retry.
    A timed-out attempt counts as a failed attempt inside its
    interval, so the timeout adds nothing on top.
    """
    start = parse_duration(healthcheck.get("start_period", "0s"))
    interval = parse_duration(healthcheck.get("interval", "0s"))
    retries = int(healthcheck.get("retries", 0))
    return start + interval * retries


def parse_dockerfile_healthcheck(text: str) -> dict | None:
    """Parse the HEALTHCHECK instruction out of a Dockerfile, if any.

    Returns a compose-shaped dict (test/interval/timeout/retries/
    start_period) so the same budget check applies, or None when
    the Dockerfile has no HEALTHCHECK. Line continuations (a
    trailing backslash — the api/worker Dockerfiles split the
    HEALTHCHECK over two lines) are joined before parsing.
    """
    joined = re.sub(r"\\\n\s*", " ", text)
    for line in joined.splitlines():
        stripped = line.strip()
        if not stripped.startswith("HEALTHCHECK"):
            continue
        flags = dict(_HEALTHCHECK_FLAG.findall(stripped))
        if "CMD" not in stripped:
            return None
        test = stripped.split("CMD", 1)[1].strip()
        return {
            "test": ["CMD-SHELL", test],
            "interval": flags.get("interval", "30s"),
            "timeout": flags.get("timeout", "30s"),
            "start_period": flags.get("start-period", "0s"),
            "retries": int(flags.get("retries", "3")),
        }
    return None


def _check_one_healthcheck(name: str, healthcheck: dict | None, failures: list[str]) -> None:
    if not isinstance(healthcheck, dict):
        failures.append(f"service {name!r} has no healthcheck")
        return
    test = healthcheck.get("test")
    if not isinstance(test, list) or not test or test[0] not in ("CMD", "CMD-SHELL"):
        failures.append(f"service {name!r} healthcheck has no CMD/CMD-SHELL test")
        return
    worst = worst_case_seconds(healthcheck)
    if worst > BUDGET_SECS:
        failures.append(
            f"service {name!r} healthcheck worst case {worst:.0f}s exceeds "
            f"the {BUDGET_SECS}s smoke budget"
        )


def _check_depends_on(name: str, service: dict, failures: list[str]) -> None:
    depends = service.get("depends_on") or {}
    if not isinstance(depends, dict):
        failures.append(f"service {name!r} depends_on is not a mapping")
        return
    if name in ("api", "worker"):
        for dep in DATA_DEPS:
            if depends.get(dep, {}).get("condition") != "service_healthy":
                failures.append(
                    f"service {name!r} does not wait for {dep} service_healthy"
                )
        if depends.get("bootstrap-lifecycle", {}).get("condition") != "service_completed_successfully":
            failures.append(
                f"service {name!r} does not wait for bootstrap "
                "service_completed_successfully"
            )
    if name == "caddy" and depends.get("api", {}).get("condition") != "service_healthy":
        failures.append("service 'caddy' does not wait for api service_healthy")


def check_services(
    services: dict, image_healthchecks: dict[str, dict | None]
) -> list[str]:
    """Check one rendered compose service map. Returns failure messages."""
    failures: list[str] = []
    for name, service in services.items():
        if not isinstance(service, dict):
            failures.append(f"service {name!r} is not a mapping")
            continue
        if name in ONE_SHOT:
            if service.get("restart") != "no":
                failures.append(f"one-shot service {name!r} must keep restart: \"no\"")
            continue
        if name in IMAGE_HEALTHCHECK:
            _check_one_healthcheck(name, image_healthchecks.get(name), failures)
        else:
            _check_one_healthcheck(name, service.get("healthcheck"), failures)
        _check_depends_on(name, service, failures)
    return failures


def render_compose(files: list[Path], profile: str, env_file: Path) -> dict:
    """Render `docker compose config` for the given layering + profile."""
    cmd = ["docker", "compose"]
    for f in files:
        cmd += ["-f", str(f)]
    cmd += ["--profile", profile, "--env-file", str(env_file), "config"]
    out = subprocess.run(cmd, capture_output=True, text=True, check=True)
    data = yaml.safe_load(out.stdout)
    if not isinstance(data, dict) or not isinstance(data.get("services"), dict):
        raise ValueError(f"rendered config for profile {profile!r} has no services")
    return data["services"]


def check(
    base: Path,
    dev: Path,
    prod: Path,
    api_dockerfile: Path,
    worker_dockerfile: Path,
    env_file: Path,
) -> list[str]:
    """Render both profiles and check them. Returns failure messages."""
    failures: list[str] = []
    try:
        image_healthchecks = {
            "api": parse_dockerfile_healthcheck(api_dockerfile.read_text()),
            "worker": parse_dockerfile_healthcheck(worker_dockerfile.read_text()),
        }
    except OSError as e:
        return [f"cannot read Dockerfile: {e}"]
    for label, files, profile in (
        ("dev", [base, dev], "dev"),
        ("prod", [base, prod], "prod"),
    ):
        try:
            services = render_compose(files, profile, env_file)
        except (subprocess.CalledProcessError, ValueError, yaml.YAMLError) as e:
            failures.append(f"{label} render failed: {e}")
            continue
        for failure in check_services(services, image_healthchecks):
            failures.append(f"{label}: {failure}")
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parent.parent
    parser.add_argument("--base", type=Path, default=root / "ops" / "compose.base.yml")
    parser.add_argument("--dev", type=Path, default=root / "ops" / "compose.dev.yml")
    parser.add_argument("--prod", type=Path, default=root / "ops" / "compose.prod.yml")
    parser.add_argument(
        "--api-dockerfile", type=Path, default=root / "Dockerfile.api"
    )
    parser.add_argument(
        "--worker-dockerfile", type=Path, default=root / "Dockerfile.worker"
    )
    parser.add_argument(
        "--env-file", type=Path, default=root / ".env.example",
        help="env file for the render (the example's dev defaults)",
    )
    args = parser.parse_args(argv)
    failures = check(
        args.base, args.dev, args.prod,
        args.api_dockerfile, args.worker_dockerfile, args.env_file,
    )
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print("OK: every compose service has a healthcheck inside the 120s budget")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
