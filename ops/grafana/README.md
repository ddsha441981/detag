# Grafana dashboards as code (P0-030, NFR-62)

## Files

| Path | What it is |
| --- | --- |
| `dashboards/api-golden.json` | API golden signals: request rate, 5xx share, latency p50/p95/p99, in-flight, build info |
| `dashboards/queue.json` | Queue health: depth per queue, enqueue/dequeue rate, stage duration p50/p95 |
| `dashboards/jobs.json` | Job pipeline health: stage p95, queue depth, worker CPU/memory |
| `provisioning/datasources/prometheus.yml` | The Prometheus datasource (`${DS_PROMETHEUS}` in every query) |
| `provisioning/dashboards/detags.yml` | Dashboard provider: loads the JSON files, re-reads every 10 s |

## How provisioning works

Grafana reads the provisioning files at startup. The datasource
points at the `prometheus` compose service; the dashboard provider
loads every `dashboards/*.json` and re-reads the directory every
`updateIntervalSeconds: 10`. **Dashboards are code**: edit the JSON,
and the live dashboard follows within 10 seconds. `allowUiUpdates`
is `false`, so a change made in the Grafana UI is overwritten by the
file — if a panel needs changing, change the JSON (and run
`just dashboards-check`).

## Reload

- **Automatic:** any edit to a dashboard JSON is picked up within 10 s.
- **Manual:** Grafana UI → Dashboards → Browse, or
  `POST /api/admin/provisioning/dashboards/reload` (admin API key).
- **Full restart:** `docker compose restart grafana` re-reads the
  provisioning files from scratch.

## What is NOT here yet (honest gaps)

- **"Visible in Grafana" is not verified in this ticket.** No compose
  file exists yet (E0.9, P0-085..P0-090) and no Grafana runs on this
  VM, so the dashboards are validated statically instead — see
  `scripts/validate_dashboards.py` (`just dashboards-check`, CI step
  in `.github/workflows/ci.yml`). Live visibility is re-verified when
  the compose stack lands; see ASSUMPTION-004 in
  `docs/09-STATE/ISSUES.md`.
- **Job outcome panels** (success rate, quality-gate pass rate) are
  absent on purpose: their series land with the E0.8 job state
  machine (P0-075+) and the Phase 2 quality gates. The jobs
  dashboard carries a text panel saying so instead of a fake
  "No data" query panel.
- **Storage / Redis / backup panels** (runbook §observability) land
  with E0.4/E0.5 storage work and the backup job — same rule: no
  invented series.

## Validation

```
just dashboards-check   # python3 scripts/validate_dashboards.py
```

Checks: valid JSON, title/uid/schemaVersion, unique uids, panels
with title/type/gridPos/targets, and — the important one — every
PromQL expression references only metric families the code actually
registers. Add a series to `ALLOWED_METRICS` in the validator only
in the ticket that registers it.
