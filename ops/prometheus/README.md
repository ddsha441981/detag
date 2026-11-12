# Prometheus config + alerts (P0-031, NFR-62)

## Files

| Path | What it is |
| --- | --- |
| `prometheus.yml` | Global intervals, `detag-api` + `prometheus` scrape jobs, rule file |
| `alerts.yml` | Active alert rules + commented pending runbook rules |
| `README.md` | This file |

## Scrape jobs

| Job | Target | What it scrapes |
| --- | --- | --- |
| `detag-api` | `localhost:9090` | The api's internal `/metrics` (P0-026): HTTP, queue, build info, process collector |
| `prometheus` | `localhost:9091` | Prometheus itself |

Targets use `localhost` because the dev compose stack runs with
host networking (ADR-0014) and the api's `/metrics` binds loopback
(ASSUMPTION-003). E0.9 revisits the targets if bridge networking
lands (then `detag-api` becomes the `api` service name).

**Port note:** Prometheus itself listens on **9091** in compose
(`--web.listen-address=:9091`) so it does not clash with the api's
9090 metrics port.

## Alerts

Active rules (thresholds per the runbook):

| Alert | Fires when | Severity |
| --- | --- | --- |
| `DetagApiDown` | `up{job="detag-api"} == 0` for 2m | page |
| `DetagApiHighErrorRate` | 5xx share > 5% for 5m | page |

`DetagApiDown` is the proxy for the runbook's "readyz fail > 2 min":
there is no `/readyz` probe series yet (the endpoint lands with the
API spec), so scrape health stands in until then.

Pending runbook alerts — queue age p95 > 60 s, job failure > 5% /
15 min, redis down, disk > 80%, backup older than 26 h — are listed
as **commented rules in `alerts.yml`** with their intended queries
and landing tickets. They are deliberately not active rules: a rule
over a nonexistent series evaluates to no data and would silently
never fire. Uncomment each one in the ticket that registers its
series.

Alertmanager is **disabled by default** (no `alerting` section in
`prometheus.yml`): rules evaluate and are visible in the Prometheus
UI/API, but there is nowhere to send notifications until E0.9 wires
an Alertmanager/receiver.

## Validation

```
just prometheus-check   # static validator + promtool, when installed
```

- `scripts/validate_prometheus.py` — YAML structure, scrape jobs,
  rule completeness (expr, `for`, severity, summary), PromQL
  allow-list (only series the code registers + `up`). Enforced in CI.
- `promtool check config` / `promtool check rules` — the real
  Prometheus parser. `just prometheus-check` runs promtool when the
  binary is on PATH (download the Prometheus release tarball and put
  `promtool` on PATH; it is not vendored in the repo). If promtool
  is absent, the recipe prints a warning and relies on the static
  validator — the gap is honest, not hidden.

## What is NOT verified yet

- **"Rules load" is verified by promtool + the static validator, not
  by a running Prometheus.** No Prometheus runs on this VM yet and
  the compose stack lands in E0.9 — see ASSUMPTION-005 in
  `docs/09-STATE/ISSUES.md`. Live loading is re-verified with the
  compose stack.
- **"Targets up" is likewise unverified live** — the targets are the
  api's `/metrics` (verified serving by P0-026's tests) and
  Prometheus itself; both are confirmed up when the stack lands.
