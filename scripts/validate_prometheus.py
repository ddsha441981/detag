"""Validate Prometheus config + alert rules (P0-031, NFR-62).

Checks ops/prometheus/prometheus.yml and ops/prometheus/alerts.yml:
  - parse as YAML;
  - prometheus.yml: global scrape interval set, >= 1 scrape job,
    every job has >= 1 static target, rule_files entries exist on
    disk, alertmanager is not configured (disabled by default per
    the ticket spec — rules evaluate, they just have nowhere to send
    until E0.9 wires notifications);
  - alerts.yml: groups with unique alert names, every rule has an
    expr, a `for` duration, a severity label and a summary
    annotation, and every PromQL expression references only metric
    families the code registers (ALLOWED_METRICS from
    validate_dashboards, plus `up` which Prometheus itself provides).

This is the static half of the ACs. The live half ("rules load",
"targets up") is checked with promtool (`just prometheus-check`
also runs promtool when it is installed — see
ops/prometheus/README.md); until the compose stack lands (E0.9),
live targets are unverified — see ASSUMPTION-005.

Run: python3 scripts/validate_prometheus.py
Exit 0 when all checks pass, 1 otherwise (CI runs this in
.github/workflows/ci.yml).
"""

import re
import sys
from pathlib import Path

from validate_dashboards import ALLOWED_METRICS, _metric_names_in_expr

# `up` is provided by Prometheus itself for every scrape target.
RULE_ALLOWED_METRICS = ALLOWED_METRICS | {"up"}

DURATION_RE = re.compile(r"^\d+[smhdw](\d+[smhdw])?$")


def _load_yaml(path: Path):
    import yaml

    return yaml.safe_load(path.read_text(encoding="utf-8"))


def validate_prometheus_config(path: Path) -> list[str]:
    errors: list[str] = []
    try:
        config = _load_yaml(path)
    except Exception as exc:  # noqa: BLE001 — report, don't crash
        return [f"{path}: invalid YAML: {exc}"]
    if not isinstance(config, dict):
        return [f"{path}: config must be a YAML mapping"]
    global_config = config.get("global") or {}
    if not isinstance(global_config, dict) or not global_config.get(
        "scrape_interval"
    ):
        errors.append(f"{path}: 'global.scrape_interval' must be set")
    if "alerting" in config and config["alerting"]:
        errors.append(
            f"{path}: 'alerting' must stay unset (alertmanager disabled "
            "by default, P0-031)"
        )
    for rule_file in config.get("rule_files") or []:
        if not (path.parent / str(rule_file)).is_file():
            errors.append(
                f"{path}: rule file '{rule_file}' does not exist next "
                "to prometheus.yml"
            )
    jobs = config.get("scrape_configs")
    if not isinstance(jobs, list) or not jobs:
        errors.append(f"{path}: 'scrape_configs' must be a non-empty list")
        return errors
    seen_jobs: set[str] = set()
    for job in jobs:
        if not isinstance(job, dict):
            errors.append(f"{path}: scrape job must be a mapping")
            continue
        name = job.get("job_name")
        if not isinstance(name, str) or not name:
            errors.append(f"{path}: scrape job missing 'job_name'")
            continue
        if name in seen_jobs:
            errors.append(f"{path}: duplicate job_name '{name}'")
        seen_jobs.add(name)
        targets = []
        for static in job.get("static_configs") or []:
            if isinstance(static, dict):
                targets.extend(static.get("targets") or [])
        if not targets:
            errors.append(
                f"{path}: job '{name}' has no static_configs targets"
            )
    return errors


def validate_alerts(path: Path) -> list[str]:
    errors: list[str] = []
    try:
        doc = _load_yaml(path)
    except Exception as exc:  # noqa: BLE001 — report, don't crash
        return [f"{path}: invalid YAML: {exc}"]
    if not isinstance(doc, dict):
        return [f"{path}: alerts file must be a YAML mapping"]
    groups = doc.get("groups")
    if not isinstance(groups, list) or not groups:
        return [f"{path}: 'groups' must be a non-empty list"]
    seen_alerts: set[str] = set()
    for group in groups:
        if not isinstance(group, dict):
            errors.append(f"{path}: group must be a mapping")
            continue
        for rule in group.get("rules") or []:
            if not isinstance(rule, dict):
                errors.append(f"{path}: rule must be a mapping")
                continue
            name = rule.get("alert")
            if not isinstance(name, str) or not name:
                errors.append(f"{path}: rule missing 'alert' name")
                continue
            if name in seen_alerts:
                errors.append(f"{path}: duplicate alert name '{name}'")
            seen_alerts.add(name)
            where = f"{path}: alert '{name}'"
            expr = rule.get("expr")
            if not isinstance(expr, str) or not expr.strip():
                errors.append(f"{where}: missing 'expr'")
            else:
                for metric in sorted(_metric_names_in_expr(expr)):
                    if metric not in RULE_ALLOWED_METRICS:
                        errors.append(
                            f"{where}: expr queries unknown metric "
                            f"'{metric}' (not registered by the code)"
                        )
            duration = rule.get("for")
            if not isinstance(duration, str) or not DURATION_RE.match(duration):
                errors.append(
                    f"{where}: 'for' must be a duration like '2m'"
                )
            labels = rule.get("labels") or {}
            if not isinstance(labels, dict) or "severity" not in labels:
                errors.append(f"{where}: labels must include 'severity'")
            annotations = rule.get("annotations") or {}
            if not isinstance(annotations, dict) or not annotations.get(
                "summary"
            ):
                errors.append(
                    f"{where}: annotations must include 'summary'"
                )
    if not seen_alerts:
        errors.append(f"{path}: no alert rules defined")
    return errors


def main() -> int:
    root = Path(__file__).resolve().parent.parent / "ops" / "prometheus"
    errors = validate_prometheus_config(root / "prometheus.yml")
    errors.extend(validate_alerts(root / "alerts.yml"))
    if errors:
        print(f"prometheus: {len(errors)} error(s):", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("prometheus: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
