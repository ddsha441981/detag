"""Unit tests for scripts/check_compose_health.py (P0-088).

Run: python3 -m unittest discover -s scripts -p "test_*.py"
"""

import unittest

from check_compose_health import (
    check_services,
    parse_dockerfile_healthcheck,
    parse_duration,
    worst_case_seconds,
)

API_HEALTHCHECK = {
    "test": ["CMD-SHELL", "curl -f http://localhost:8080/healthz"],
    "interval": "10s",
    "timeout": "2s",
    "start_period": "10s",
    "retries": 3,
}

WORKER_HEALTHCHECK = {
    "test": ["CMD-SHELL", "curl -f http://localhost:9092/metrics"],
    "interval": "10s",
    "timeout": "2s",
    "start_period": "10s",
    "retries": 3,
}


def _hc(test="CMD-SHELL", start="10s", interval="5s", retries=10):
    return {
        "test": [test, "true"],
        "interval": interval,
        "timeout": "5s",
        "start_period": start,
        "retries": retries,
    }


def _data_deps():
    return {
        "postgres": {"condition": "service_healthy"},
        "redis": {"condition": "service_healthy"},
        "minio": {"condition": "service_healthy"},
        "bootstrap": {"condition": "service_completed_successfully"},
    }


def _dev_services():
    return {
        "postgres": {"healthcheck": _hc()},
        "redis": {"healthcheck": _hc()},
        "minio": {"healthcheck": _hc()},
        "bootstrap": {"restart": "no", "depends_on": {"minio": {"condition": "service_healthy"}}},
        "api": {"depends_on": _data_deps()},
        "worker": {"depends_on": _data_deps()},
        "web": {"healthcheck": _hc(start="5s")},
        "mailhog": {"healthcheck": _hc()},
    }


def _prod_services():
    services = _dev_services()
    del services["web"]
    del services["mailhog"]
    services["caddy"] = {
        "healthcheck": _hc(),
        "depends_on": {"api": {"condition": "service_healthy"}},
    }
    return services


IMAGE_HEALTHCHECKS = {"api": API_HEALTHCHECK, "worker": WORKER_HEALTHCHECK}


class TestParseDuration(unittest.TestCase):
    def test_seconds(self):
        self.assertEqual(parse_duration("5s"), 5)
        self.assertEqual(parse_duration("0s"), 0)

    def test_compound(self):
        self.assertEqual(parse_duration("1m30s"), 90)
        self.assertEqual(parse_duration("500ms"), 0.5)
        self.assertEqual(parse_duration("2m"), 120)


class TestWorstCase(unittest.TestCase):
    def test_postgres_shape(self):
        # 10s start + 5s * 10 retries = 60s, inside the budget.
        self.assertEqual(worst_case_seconds(_hc()), 60)

    def test_api_shape(self):
        # 10s start + 10s * 3 retries = 40s.
        self.assertEqual(worst_case_seconds(API_HEALTHCHECK), 40)


class TestParseDockerfileHealthcheck(unittest.TestCase):
    def test_api_dockerfile_shape(self):
        text = (
            "FROM x\n"
            "HEALTHCHECK --interval=10s --timeout=2s --start-period=10s --retries=3 \\\n"
            "  CMD curl -f http://localhost:8080/healthz\n"
        )
        parsed = parse_dockerfile_healthcheck(text)
        self.assertIsNotNone(parsed)
        self.assertEqual(parsed["interval"], "10s")
        self.assertEqual(parsed["start_period"], "10s")
        self.assertEqual(parsed["retries"], 3)
        self.assertEqual(worst_case_seconds(parsed), 40)

    def test_no_healthcheck(self):
        self.assertIsNone(parse_dockerfile_healthcheck("FROM x\nRUN true\n"))


class TestCheckServices(unittest.TestCase):
    def test_dev_render_passes(self):
        self.assertEqual(check_services(_dev_services(), IMAGE_HEALTHCHECKS), [])

    def test_prod_render_passes(self):
        self.assertEqual(check_services(_prod_services(), IMAGE_HEALTHCHECKS), [])

    def test_missing_healthcheck_fails(self):
        services = _dev_services()
        del services["web"]["healthcheck"]
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("web" in f and "no healthcheck" in f for f in failures))

    def test_missing_image_healthcheck_fails(self):
        image = {"api": None, "worker": WORKER_HEALTHCHECK}
        failures = check_services(_dev_services(), image)
        self.assertTrue(any("api" in f and "no healthcheck" in f for f in failures))

    def test_over_budget_fails(self):
        services = _dev_services()
        services["web"]["healthcheck"] = _hc(start="10s", interval="30s", retries=10)
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("web" in f and "exceeds" in f for f in failures))

    def test_api_missing_bootstrap_dep_fails(self):
        services = _dev_services()
        del services["api"]["depends_on"]["bootstrap"]
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("api" in f and "bootstrap" in f for f in failures))

    def test_api_missing_healthy_dep_fails(self):
        services = _dev_services()
        services["api"]["depends_on"]["redis"]["condition"] = "service_started"
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("api" in f and "redis" in f for f in failures))

    def test_caddy_missing_api_dep_fails(self):
        services = _prod_services()
        del services["caddy"]["depends_on"]["api"]
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("caddy" in f and "api" in f for f in failures))

    def test_bootstrap_must_stay_one_shot(self):
        services = _dev_services()
        services["bootstrap"]["restart"] = "unless-stopped"
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("bootstrap" in f for f in failures))

    def test_non_cmd_test_fails(self):
        services = _dev_services()
        services["web"]["healthcheck"]["test"] = ["RUN", "true"]
        failures = check_services(services, IMAGE_HEALTHCHECKS)
        self.assertTrue(any("web" in f and "CMD" in f for f in failures))


if __name__ == "__main__":
    unittest.main()
