# Deployment Runbook

## First deploy (single VPS)

1. Provision Ubuntu LTS, 4 vCPU / 8 GB / 100 GB SSD, Docker + compose plugin installed.
2. Clone repo, copy `ops/compose.prod.yml`; create `.env` from `.env.example`:
   - Required: `PF_JWT_SECRET` (64B random), `PF_DATABASE_URL`, `PF_REDIS_URL`, `PF_S3_*` (or bundled MinIO), `PF_PUBLIC_BASE_URL`, `PF_STRIPE_SECRET_KEY` (+ webhook secret), SMTP vars.
   - Feature flags default OFF (razorpay, gpu, provenance_strip).
3. `docker compose up -d` → `just migrate deploy` → create admin: `docker compose exec api pf-admin-create --email you@example.com` (prints one-time setup link).
4. Caddy front: automatic HTTPS for the domain; only caddy + web + api are on the `edge` network; db/redis/minio on `internal` only.
5. Verify: `/healthz` 200, `/readyz` 200, register test user, upload test image, job succeeds, quality report shows pass.
6. Backups: cron `ops/backup.sh` (pg_dump + minio mirror to off-host storage) nightly; verify restore doc step below.

## Environment variables (PF_*)

| Var | Used by | Default (dev) | Notes |
| --- | --- | --- | --- |
| `PF_DATABASE_URL` | api, worker | postgres://pf:pf@localhost:5432/pf | |
| `PF_POSTGRES_USER/PASSWORD/DB` | postgres (compose) | pf / pf / pf | dev credentials for the compose postgres service (P0-034); must match `PF_DATABASE_URL` |
| `PF_REDIS_URL` | api, worker | redis://localhost:6379 | |
| `PF_S3_ENDPOINT`, `PF_S3_BUCKET`, `PF_S3_ACCESS_KEY`, `PF_S3_SECRET_KEY`, `PF_S3_REGION` | storage | MinIO localhost:9000 | private bucket required |
| `PF_JWT_SECRET` | api | dev-fixed | 64B in prod; rotation = dual-verify window |
| `PF_PUBLIC_BASE_URL` | api | http://localhost:8080 | used in signed URLs |
| `PF_API_PORT` | api | 8080 | port the api binds on 127.0.0.1 |
| `PF_METRICS_PORT` | api | 9090 | port the internal /metrics server binds on 127.0.0.1 |
| `PF_STRIPE_SECRET_KEY`, `PF_STRIPE_WEBHOOK_SECRET` | billing | test keys | live keys = owner action |
| `PF_RAZORPAY_KEY`, `PF_RAZORPAY_SECRET`, `PF_FEATURE_RAZORPAY` | billing | off | |
| `PF_SMTP_HOST`, `PF_SMTP_PORT`, `PF_SMTP_USER`, `PF_SMTP_PASS`, `PF_MAIL_FROM` | mailer | MailHog | |
| `PF_ENGINE_MODELS_DIR`, `PF_ENGINE_GPU`, `PF_ENGINE_TILES_MAX` | worker/engine | ./models, off, 1024 | models auto-download + sha256 verify |
| `PF_RESULTS_TTL_FREE/PRO/BUSINESS_H` | cleanup | 6/24/72 | plan TTLs |
| `PF_UPLOAD_PART_URL_TTL_SECS` | storage | 600 | presigned PUT part URL lifetime; must stay under the 15-min access-token TTL (P0-055) |
| `PF_RATELIMIT_*` | api | per-plan defaults | tuned Phase 5 |
| `PF_FEATURE_PROVENANCE_STRIP` | api | off | legal gate required |
| `PF_LOG_LEVEL`, `PF_OTEL_ENDPOINT` | all | info, empty | OTLP/HTTP; `/v1/traces` appended when the endpoint has no path |

## Release flow (cocogitto, P0-010)

Releases are SemVer tags (`vMAJOR.MINOR.PATCH`) cut from `main` with
cocogitto (`cog`, config in `cog.toml`). The changelog is generated from
conventional commits — no hand-written entries.

1. **Gate:** `docs/06-DELIVERY/02-release-checklist.md` top-to-bottom.
   Anything red blocks the release (owner-input items get listed, not skipped).
2. **Dry run first:** `just release-dry-run`
   - `cog bump --auto --dry-run` prints the next version the commits imply
     (`feat` → minor, `fix`/`perf` → patch, `BREAKING CHANGE` → major).
     It writes nothing.
   - `just changelog` regenerates `CHANGELOG.md` from history; review the
     diff. The generated file is committed as part of a real release (or
     as the P0-010 stub) — never hand-edit its entries.
3. **Cut the release:** `cog bump --auto` (bumps `Cargo.toml` versions,
   writes `CHANGELOG.md`, creates the `vX.Y.Z` tag) — then push the tag.
   Never hand-edit the version in `Cargo.toml` or the tag name; `cog` owns both.
4. **Deploy the tag:** build images from the tag (not from `main`),
   follow "Upgrades" below, and verify with the post-deploy smoke.
5. **After:** the tag is the rollback point (previous image tags are kept).

Pre-1.0 rule: while the workspace version is `0.x`, `feat` bumps the minor
and `fix` bumps the patch; breaking changes bump the minor, never the major,
until v1.0.0 is declared (see the release checklist).

1. Read CHANGELOG + migration notes; backup first (`ops/backup.sh` + verify one restore).
2. `docker compose pull && docker compose up -d` (api/worker restart-gated on health).
3. `just migrate deploy` (expand-only at night; contract steps need owner-ok).
4. Post-deploy smoke: healthz/readyz + one image job + webhook test event.

## Rollback

1. `docker compose` previous image tags (keep last 3 tagged images).
2. Migrations: additive ones are compatible with previous app version — rolling back the image is enough. Contract migrations are never released in the same window as features.
3. If queue stuck: `just queue:requeue-stuck` (visibility-timeout sweep).

## Restore drill (NFR-22, rehearsed once in Phase 5)

1. Empty scratch Postgres + MinIO bucket.
2. Restore `pg_dump` latest; mirror storage back.
3. Boot stack with scratch config; login as test user; verify a historical job's artifacts (within TTL) render.
4. Record time-to-restore in night report; target < 30 min.

## Redis

Redis is a cache, a rate limiter, and a job-event bus — never the
source of truth. Postgres owns durable state; every Redis entry is
re-derivable (flags from the flag store, usage counters from the
usage rows, rate-limit buckets from the next check, cache entries
from their loader). Losing Redis costs latency and burst control,
never data. The service is defined in `ops/compose.redis.yml`
(`redis:7-alpine`, folded into `compose.base.yml` by E0.9, P0-085).

### Key naming

Every key is `pf:{namespace}:{key}` (see `crates/core/src/redis/keys.rs`).
Callers use the typed helpers (`flags_key`, `usage_key`,
`rate_limit_key`, `cache_key`, `job_events_channel`,
`idempotency_key`, `job_key`) — never a hand-built string. A key with
whitespace or an empty key panics: that is a programming error, not
a runtime one.

| Namespace | Pattern | Used by |
| --- | --- | --- |
| `flags` | `pf:flags:{flag}` | Feature-flag cache (P0-022, E0.8) |
| `usage` | `pf:usage:{user}:{YYYY-MM-DD}` | Daily usage-counter mirrors (Phase 1) |
| `ratelimit` | `pf:ratelimit:{scope}:{id}` | Rate-limit buckets (P0-048) |
| `cache` | `pf:cache:{kind}:{id}` | `get_or_set` cache entries (P0-049) |
| `pubsub` | `pf:pubsub:job:{job_id}` | Job event channels (P0-050) |
| `idempotency` | `pf:idempotency:{key}` | Idempotency response cache |
| `jobs` | `pf:jobs:{job_id}` | Job status/progress cache |

### TTL conventions

| Entry | TTL |
| --- | --- |
| Flag cache | `TTL_FLAG_CACHE` — 30 s |
| Usage-counter mirror | `TTL_USAGE_COUNTER` — 48 h (outlives the day it counts + rollover grace; the durable row lives 400 d in Postgres) |
| Result/status cache | Capped by `TTL_RESULT_MAX` — 72 h (the Business plan TTL; a shorter plan TTL caps its own entries). Plan result TTLs: Free 6 h / Pro 24 h / Business 72 h (`PF_RESULTS_TTL_FREE/PRO/BUSINESS_H`) |
| Rate-limit bucket | Exactly its window — no shared constant |
| Cache entry | Its caller's TTL — no shared constant |
| Idempotency entry | Until the row's `expires_at` |
| Pubsub channel | None — channels are ephemeral |

TTLs that are not constants are deliberately absent: a shared
constant for those would be wrong the first time a caller needed a
different value.

### Memory expectations

- **No persistence, by design.** `--appendonly no`, `--save ""`,
  no volume: a restart must never replay stale entries. Cache
  data is ephemeral — `docker compose down` loses it and nothing
  breaks.
- **Cap + eviction (dev):** `--maxmemory 256mb`
  `--maxmemory-policy allkeys-lru`. When full, the
  least-recently-used keys go first. That is safe for every
  namespace: usage counters and the flags cache are re-derived
  from Postgres, rate-limit buckets are re-created on the next
  check, singleflight entries are ephemeral, and pubsub channels
  hold nothing on disk. 256 MB is a dev default (the VM has
  ~7.7 GB and the cache must never starve the api/worker);
  production sizing is a deploy-time decision, not a code change.
- **What to watch:** `detag_cache_hits_total` /
  `detag_cache_misses_total` by kind (P0-049) on `/metrics`, plus
  `redis-cli INFO memory` (`used_memory_human`,
  `evicted_keys`). A climbing eviction rate with a flat hit rate
  means the cap is too small for the working set — raise
  `maxmemory` in `ops/compose.redis.yml`, not the TTLs.

### Degraded mode (NFR-13)

NFR-13: *Redis failure degrades gracefully: API returns 503 +
Retry-After on job creation, reads stay up.* What that means in
practice, and what is implemented today:

- **Implemented (P0-051):** any Redis failure on a Redis-dependent
  path surfaces as typed `AppError::CacheUnavailable` — PF-5003,
  HTTP 503, PII-safe detail ("the cache is temporarily
  unavailable"). The provider error never reaches the response
  body. Failover + recovery is covered by
  `crates/core/tests/redis_integration.rs`.
- **Implemented (P0-049):** the cache is read-through — a miss
  loads from the source of truth, so a Redis outage costs a
  reload, not a wrong answer.
- **Implemented (P0-022):** the flag resolver fails closed to OFF
  when the flag store errors, and caches that answer for the TTL.
- **Not yet:** the `Retry-After` header on job creation — it lands
  with the job-creation endpoint (the endpoint does not exist
  yet). The `DetagRedisDown` alert rule is commented out in
  `ops/prometheus/alerts.yml` because it needs a redis_exporter
  scrape job (E0.5, P0-044+); `/readyz` does not exist yet either — the
  `redis_health` probe in core (P0-046) is what it will call.
  Until then, Redis health is the compose healthcheck plus
  `redis-cli ping`.

### Operating it

```bash
# Is it up? (compose healthcheck runs the same command)
docker compose -f ops/compose.redis.yml ps
redis-cli ping                                   # PONG
redis-cli INFO memory | grep -E 'used_memory_human|evicted_keys|maxmemory'
redis-cli DBSIZE                                 # key count
redis-cli --scan --pattern 'pf:ratelimit:*' | wc -l   # keys per namespace
```

- **Restart:** `docker compose -f ops/compose.redis.yml down && docker compose -f ops/compose.redis.yml up -d --wait` — data loss is by design (see above). Dev only, while E0.9 is pending: the native `redis-server` on this VM (ASSUMPTION-008) is started manually and does not survive a VM reset — reinstall/restart per ASSUMPTION-010.
- **Flush:** `FLUSHDB` is safe (everything is re-derivable) but it
  also wipes rate-limit buckets, so the next minute admits a
  burst up to the full per-window limit. Do it in low traffic and
  say so in the incident log.
- **Redis down:** `docker compose ps`, then the redis logs. While
  it is down, Redis-dependent writes answer PF-5003 (503) and
  reads fall back to Postgres; do not restart the api to "fix"
  it — the api recovers on its own once Redis answers again
  (failover + recovery is tested, P0-051). Never point
  `PF_REDIS_URL` at a shared or public Redis: dev binds loopback
  only, no password; production needs a password and the
  `internal` network (see "First deploy" above).

## Observability

### Dashboards (Grafana, as code)

Dashboards live in `ops/grafana/dashboards/` and are provisioned
automatically (re-read every 10 s; `allowUiUpdates` is `false` — a
change made in the Grafana UI is overwritten, so edit the JSON and
run `just dashboards-check`). Until the E0.9 compose stack lands
they are validated statically only (see ASSUMPTION-004); there is no
live Grafana to look at yet.

| Dashboard | What to read | What "good" looks like |
| --- | --- | --- |
| **Detag API — golden signals** (`api-golden.json`) | Request rate by route, 5xx share, latency p50/p95/p99, in-flight requests, build info | 5xx share near 0; p95 flat while rate rises; in-flight drains after deploys |
| **Detag queues** (`queue.json`) | Depth per queue, enqueue/dequeue rate, stage duration p50/p95, stage runs observed | Depth near 0 between bursts; dequeue keeps up with enqueue; stage p95 stable |
| **Detag jobs** (`jobs.json`) | Stage duration p95 by stage, queue depth (all queues), worker CPU/memory | No single stage dominates p95; worker CPU/memory flat, not climbing |

Reading a bad dashboard: p95 rising *with* queue depth means the
worker is the bottleneck (stuck/slow jobs — check the queue
dashboard's stage durations); p95 rising with *flat* queue depth
means the api or a dependency is slow (check the api-golden error
rate, then the api logs). A depth that grows while dequeue rate is
flat means the worker is down or not consuming — check
`docker compose ps` before anything else.

Honest gaps (no invented panels): job-outcome panels (success rate,
quality-gate pass rate) land with the E0.8 job state machine and
Phase 2 quality gates; storage / Redis / backup panels land with
E0.4/E0.5 storage work and the backup job. The jobs dashboard says
so in a text panel instead of showing a fake "No data" query.

### Alerts (Prometheus, `ops/prometheus/`)

Prometheus scrapes the api's `/metrics` (`detag-api` job,
`localhost:9090` by default — `PF_METRICS_PORT`) and itself
(`localhost:9091`). There is no Alertmanager yet, so alerts surface
wherever Prometheus is watched; the `for:` window is the time the
condition must hold before the alert fires.

| Alert | Severity | What it means | First steps |
| --- | --- | --- | --- |
| `DetagApiDown` (`up{job="detag-api"} == 0`, 2m) | page | Prometheus cannot scrape the api's `/metrics` — the api is down or its metrics port is blocked. Proxy for "readyz fail > 2 min"; there is no `/readyz` probe series yet. | `docker compose ps`; api logs; check the metrics port is reachable. `/readyz` reports db/redis/storage/model health once it lands. |
| `DetagApiHighErrorRate` (5xx share > 5%, 5m) | page | More than 5% of api requests return 5xx. | Api logs + api-golden error-rate panel; likely a down dependency (db/redis/storage) or a bad deploy. |

Pending (commented rules in `ops/prometheus/alerts.yml` — not
active, because a rule over a series that does not exist yet would
silently never fire; each is uncommented in the ticket that
registers its series): `DetagQueueAgeHigh` (queue age p95 > 60 s,
E0.8), `DetagJobFailureRateHigh` (job failure > 5% / 15 min, E0.8),
`DetagRedisDown` (E0.5), `DetagDiskUsageHigh` (disk > 80%, E0.9),
`DetagBackupStale` (backup older than 26 h, Phase 5).

### Trace-id to request correlation

The request id is the correlation id — there is no separate trace
id to look up. Flow:

1. The client sends `x-request-id` (any UUID version) or none; the
   api echoes the id back on every response as `x-request-id`, and
   generates a fresh UUIDv7 when the client sends none or a bad
   value (P0-019).
2. Every error body (`application/problem+json`) carries the same id
   in its `request_id` field — ask the user for it, or copy it from
   the response header.
3. Every JSON log line inside that request carries the id in its
   span context (`http_request` span: `request_id`, `route`,
   `method`); job logs additionally carry `job_id` (`job` span).
   Search the logs with the id:
   `docker compose logs api | grep '<request-id>'`
   (logs are JSON; the id appears as a field value on every line of
   the request).
4. When `PF_OTEL_ENDPOINT` is set, the same `http_request` span —
   with its `request_id` field — is exported via OTLP, so a trace
   in the backend and the log lines share the id. When it is empty,
   logs are the only record.

Access-log sampling (P0-019): the per-request `request completed`
event is emitted always for errors (status >= 400) and slow
requests (> 250 ms), and for a deterministic 1% of successes (a
hash of the request id, so the same id always decides the same
way). The span itself exists for every request regardless of
sampling — the id is always in the logs, even when the access event
is not.

Log hygiene (P0-032): logs never contain emails, passwords, tokens,
or raw IPs — values are redacted (`[email]`, `[redacted]`) or
hashed (`[ip:…]`) before they are emitted. If you ever see a raw
one in a log line, that is a bug: file it, do not paste the line
into a ticket.

- Nightly reconciliation job writes `system` summary to `09-STATE/PROGRESS.md` when running in the dev repo context.
