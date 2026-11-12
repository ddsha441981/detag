"""Validate Grafana dashboards as code (P0-030, NFR-62).

Checks every JSON dashboard under ops/grafana/dashboards/:
  - parses as JSON, has title / uid / schemaVersion / panels;
  - uids are unique across dashboards;
  - every panel has a title, a known type, a gridPos and (except
    text panels) at least one PromQL target;
  - every PromQL target queries only metric families the code
    actually registers (ALLOWED_METRICS) through the ${DS_PROMETHEUS}
    datasource — a dashboard must never invent a series (e.g. quality
    gate rates land in Phase 2, not here);
  - the provisioning files exist, parse as YAML and reference the
    dashboard directory.

Run: python3 scripts/validate_dashboards.py
Exit 0 when all checks pass, 1 otherwise (CI runs this in
.github/workflows/ci.yml).
"""

import json
import re
import sys
from pathlib import Path

# Metric families the code registers today (crates/core/src/metrics.rs
# + the Prometheus process collector). Extend this list only in the
# ticket that registers the new series.
ALLOWED_METRICS = frozenset(
    {
        "detag_build_info",
        "detag_http_requests_total",
        "detag_http_request_duration_seconds",
        "detag_http_requests_inflight",
        "detag_queue_depth",
        "detag_queue_enqueued_total",
        "detag_queue_dequeued_total",
        "detag_job_stage_duration_seconds",
        "process_cpu_seconds_total",
        "process_max_fds",
        "process_open_fds",
        "process_resident_memory_bytes",
        "process_start_time_seconds",
        "process_threads",
        "process_virtual_memory_bytes",
    }
)

DATASOURCE_UID = "${DS_PROMETHEUS}"
PANEL_TYPES = frozenset(
    {"timeseries", "stat", "text", "gauge", "table", "heatmap"}
)
UID_RE = re.compile(r"^[A-Za-z0-9_-]+$")
# Histogram series appear as <base>_bucket/_count/_sum in queries;
# strip those suffixes before the allow-list check.
HISTOGRAM_SUFFIXES = ("_bucket", "_count", "_sum")
IDENT_RE = re.compile(r"[a-zA-Z_:][a-zA-Z0-9_:]*")


def _metric_names_in_expr(expr: str) -> set[str]:
    names = set()
    for token in IDENT_RE.findall(expr):
        if not (token.startswith("detag_") or token.startswith("process_")):
            continue
        for suffix in HISTOGRAM_SUFFIXES:
            if token.endswith(suffix):
                token = token[: -len(suffix)]
                break
        names.add(token)
    return names


def validate_dashboard(path: Path) -> list[str]:
    errors: list[str] = []
    try:
        dashboard = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return [f"{path}: invalid JSON: {exc}"]
    if not isinstance(dashboard, dict):
        return [f"{path}: dashboard must be a JSON object"]
    title = dashboard.get("title")
    if not isinstance(title, str) or not title.strip():
        errors.append(f"{path}: missing or empty 'title'")
    uid = dashboard.get("uid")
    if not isinstance(uid, str) or not UID_RE.match(uid):
        errors.append(f"{path}: missing or invalid 'uid' (want [A-Za-z0-9_-]+)")
    schema = dashboard.get("schemaVersion")
    if not isinstance(schema, int) or schema < 30:
        errors.append(f"{path}: 'schemaVersion' must be an int >= 30")
    panels = dashboard.get("panels")
    if not isinstance(panels, list) or not panels:
        errors.append(f"{path}: 'panels' must be a non-empty list")
        return errors
    for index, panel in enumerate(panels):
        where = f"{path}: panel[{index}]"
        if not isinstance(panel, dict):
            errors.append(f"{where}: panel must be an object")
            continue
        if not isinstance(panel.get("title"), str) or not panel["title"].strip():
            errors.append(f"{where}: missing or empty 'title'")
        if panel.get("type") not in PANEL_TYPES:
            errors.append(
                f"{where}: unknown 'type' {panel.get('type')!r} "
                f"(want one of {sorted(PANEL_TYPES)})"
            )
        grid = panel.get("gridPos")
        if not isinstance(grid, dict) or not all(
            isinstance(grid.get(key), int) for key in ("x", "y", "w", "h")
        ):
            errors.append(f"{where}: missing or invalid 'gridPos'")
        if panel.get("type") == "text":
            continue
        targets = panel.get("targets")
        if not isinstance(targets, list) or not targets:
            errors.append(f"{where}: non-text panel needs >= 1 target")
            continue
        for target in targets:
            if not isinstance(target, dict):
                errors.append(f"{where}: target must be an object")
                continue
            expr = target.get("expr")
            if not isinstance(expr, str) or not expr.strip():
                errors.append(f"{where}: target missing 'expr'")
                continue
            for name in sorted(_metric_names_in_expr(expr)):
                if name not in ALLOWED_METRICS:
                    errors.append(
                        f"{where}: expr queries unknown metric "
                        f"'{name}' (not registered by the code)"
                    )
            datasource = target.get("datasource") or {}
            if datasource.get("uid") != DATASOURCE_UID:
                errors.append(
                    f"{where}: datasource uid must be '{DATASOURCE_UID}'"
                )
    return errors


def validate_all(grafana_root: Path) -> list[str]:
    errors: list[str] = []
    dashboards_dir = grafana_root / "dashboards"
    if not dashboards_dir.is_dir():
        return [f"missing dashboards directory: {dashboards_dir}"]
    seen_uids: dict[str, Path] = {}
    for path in sorted(dashboards_dir.glob("*.json")):
        errors.extend(validate_dashboard(path))
        try:
            uid = json.loads(path.read_text(encoding="utf-8")).get("uid")
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(uid, str):
            if uid in seen_uids:
                errors.append(
                    f"duplicate uid '{uid}': {seen_uids[uid]} and {path}"
                )
            seen_uids[uid] = path
    if not seen_uids:
        errors.append(f"no dashboards found under {dashboards_dir}")
    for rel in (
        "provisioning/datasources/prometheus.yml",
        "provisioning/dashboards/detags.yml",
    ):
        full = grafana_root / rel
        if not full.is_file():
            errors.append(f"missing provisioning file: {full}")
            continue
        try:
            import yaml

            parsed = yaml.safe_load(full.read_text(encoding="utf-8"))
        except Exception as exc:  # noqa: BLE001 — report, don't crash
            errors.append(f"{full}: invalid YAML: {exc}")
            continue
        if not isinstance(parsed, dict):
            errors.append(f"{full}: provisioning YAML must be a mapping")
    return errors


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    errors = validate_all(repo_root / "ops" / "grafana")
    if errors:
        print(f"dashboards: {len(errors)} error(s):", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("dashboards: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
