"""Unit tests for scripts/validate_prometheus.py (P0-031, NFR-62).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
(CI runs this in .github/workflows/commits.yml.)
"""

import tempfile
import unittest
from pathlib import Path

from validate_prometheus import (
    validate_alerts,
    validate_prometheus_config,
)

GOOD_CONFIG = """\
global:
  scrape_interval: 15s
  evaluation_interval: 15s
rule_files:
  - alerts.yml
scrape_configs:
  - job_name: detag-api
    static_configs:
      - targets: ['localhost:9090']
  - job_name: prometheus
    static_configs:
      - targets: ['localhost:9091']
"""

GOOD_ALERTS = """\
groups:
  - name: detag-api
    rules:
      - alert: DetagApiDown
        expr: up{job="detag-api"} == 0
        for: 2m
        labels:
          severity: page
        annotations:
          summary: Detag API scrape down
      - alert: DetagApiHighErrorRate
        expr: sum(rate(detag_http_requests_total{status=~"5.."}[5m])) / sum(rate(detag_http_requests_total[5m])) > 0.05
        for: 5m
        labels:
          severity: page
        annotations:
          summary: API 5xx share above 5%
"""


class TestValidConfig(unittest.TestCase):
    def test_good_config_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prometheus.yml"
            path.write_text(GOOD_CONFIG, encoding="utf-8")
            (Path(tmp) / "alerts.yml").write_text(GOOD_ALERTS, encoding="utf-8")
            self.assertEqual(validate_prometheus_config(path), [])

    def test_good_alerts_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "alerts.yml"
            path.write_text(GOOD_ALERTS, encoding="utf-8")
            self.assertEqual(validate_alerts(path), [])


class TestInvalidConfig(unittest.TestCase):
    def test_invalid_yaml(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prometheus.yml"
            path.write_text("global: [unclosed", encoding="utf-8")
            self.assertTrue(validate_prometheus_config(path))

    def test_missing_scrape_configs(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prometheus.yml"
            path.write_text(
                "global:\n  scrape_interval: 15s\n", encoding="utf-8"
            )
            errors = validate_prometheus_config(path)
            self.assertTrue(
                any("scrape_configs" in e for e in errors), errors
            )

    def test_job_without_targets(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prometheus.yml"
            path.write_text(
                "scrape_configs:\n  - job_name: empty\n",
                encoding="utf-8",
            )
            errors = validate_prometheus_config(path)
            self.assertTrue(any("targets" in e for e in errors), errors)

    def test_missing_rule_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "prometheus.yml"
            path.write_text(
                GOOD_CONFIG.replace("alerts.yml", "nope.yml"),
                encoding="utf-8",
            )
            errors = validate_prometheus_config(path)
            self.assertTrue(
                any("nope.yml" in e for e in errors), errors
            )


class TestInvalidAlerts(unittest.TestCase):
    def test_rule_without_for(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "alerts.yml"
            path.write_text(
                GOOD_ALERTS.replace("        for: 2m\n", ""),
                encoding="utf-8",
            )
            errors = validate_alerts(path)
            self.assertTrue(any("'for'" in e for e in errors), errors)

    def test_rule_without_severity(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "alerts.yml"
            path.write_text(
                GOOD_ALERTS.replace("          severity: page\n", ""),
                encoding="utf-8",
            )
            errors = validate_alerts(path)
            self.assertTrue(
                any("severity" in e for e in errors), errors
            )

    def test_unknown_metric_in_expr(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "alerts.yml"
            path.write_text(
                GOOD_ALERTS.replace(
                    "up{job=\"detag-api\"} == 0",
                    "detag_job_outcomes_total > 5",
                ),
                encoding="utf-8",
            )
            errors = validate_alerts(path)
            self.assertTrue(
                any("detag_job_outcomes_total" in e for e in errors),
                errors,
            )

    def test_duplicate_alert_name(self):
        import yaml

        doc = yaml.safe_load(GOOD_ALERTS)
        doc["groups"][0]["rules"].append(doc["groups"][0]["rules"][0])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "alerts.yml"
            path.write_text(yaml.safe_dump(doc), encoding="utf-8")
            errors = validate_alerts(path)
            self.assertTrue(
                any("duplicate" in e for e in errors), errors
            )
