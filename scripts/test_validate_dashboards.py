"""Unit tests for scripts/validate_dashboards.py (P0-030, NFR-62).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
(CI runs this in .github/workflows/commits.yml.)
"""

import json
import tempfile
import unittest
from pathlib import Path

from validate_dashboards import (
    ALLOWED_METRICS,
    validate_all,
    validate_dashboard,
)

MINIMAL_DASHBOARD = {
    "title": "Test",
    "uid": "detag-test",
    "schemaVersion": 39,
    "panels": [
        {
            "title": "Rate",
            "type": "timeseries",
            "gridPos": {"x": 0, "y": 0, "w": 12, "h": 8},
            "targets": [
                {
                    "expr": "sum by (route) (rate(detag_http_requests_total[5m]))",
                    "datasource": {"type": "prometheus", "uid": "${DS_PROMETHEUS}"},
                }
            ],
        }
    ],
}


def write_dashboard(directory: Path, name: str, content: dict) -> Path:
    path = directory / name
    path.write_text(json.dumps(content), encoding="utf-8")
    return path


class TestValidDashboard(unittest.TestCase):
    def test_minimal_dashboard_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", MINIMAL_DASHBOARD)
            self.assertEqual(validate_dashboard(path), [])

    def test_histogram_query_passes(self):
        dashboard = json.loads(json.dumps(MINIMAL_DASHBOARD))
        dashboard["panels"][0]["targets"][0]["expr"] = (
            "histogram_quantile(0.95, sum by (le) "
            "(rate(detag_http_request_duration_seconds_bucket[5m])))"
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", dashboard)
            self.assertEqual(validate_dashboard(path), [])

    def test_text_panel_needs_no_targets(self):
        dashboard = json.loads(json.dumps(MINIMAL_DASHBOARD))
        dashboard["panels"].append(
            {
                "title": "Note",
                "type": "text",
                "gridPos": {"x": 0, "y": 8, "w": 12, "h": 4},
                "options": {"content": "lands later"},
            }
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", dashboard)
            self.assertEqual(validate_dashboard(path), [])


class TestInvalidDashboard(unittest.TestCase):
    def test_invalid_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.json"
            path.write_text("{not json", encoding="utf-8")
            self.assertTrue(
                any("JSON" in e for e in validate_dashboard(path))
            )

    def test_missing_uid(self):
        dashboard = json.loads(json.dumps(MINIMAL_DASHBOARD))
        del dashboard["uid"]
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", dashboard)
            self.assertTrue(
                any("uid" in e for e in validate_dashboard(path))
            )

    def test_unknown_metric_in_expr(self):
        dashboard = json.loads(json.dumps(MINIMAL_DASHBOARD))
        dashboard["panels"][0]["targets"][0]["expr"] = (
            "detag_quality_gate_pass_rate"
        )
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", dashboard)
            errors = validate_dashboard(path)
            self.assertTrue(
                any("detag_quality_gate_pass_rate" in e for e in errors),
                errors,
            )

    def test_missing_panels(self):
        dashboard = json.loads(json.dumps(MINIMAL_DASHBOARD))
        dashboard["panels"] = []
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", dashboard)
            self.assertTrue(
                any("panel" in e for e in validate_dashboard(path))
            )

    def test_wrong_datasource(self):
        dashboard = json.loads(json.dumps(MINIMAL_DASHBOARD))
        dashboard["panels"][0]["targets"][0]["datasource"]["uid"] = "other"
        with tempfile.TemporaryDirectory() as tmp:
            path = write_dashboard(Path(tmp), "test.json", dashboard)
            self.assertTrue(
                any("datasource" in e for e in validate_dashboard(path))
            )


class TestValidateAll(unittest.TestCase):
    def test_duplicate_uid_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "dashboards").mkdir()
            write_dashboard(root / "dashboards", "a.json", MINIMAL_DASHBOARD)
            write_dashboard(root / "dashboards", "b.json", MINIMAL_DASHBOARD)
            errors = validate_all(root)
            self.assertTrue(
                any("duplicate uid" in e for e in errors), errors
            )

    def test_allowed_metrics_come_from_code(self):
        # The allow-list must only name series the code actually
        # registers (core metrics.rs + process collector).
        for name in (
            "detag_http_requests_total",
            "detag_queue_depth",
            "detag_job_stage_duration_seconds",
            "process_resident_memory_bytes",
        ):
            self.assertIn(name, ALLOWED_METRICS)
        self.assertNotIn("detag_quality_gate_pass_rate", ALLOWED_METRICS)
