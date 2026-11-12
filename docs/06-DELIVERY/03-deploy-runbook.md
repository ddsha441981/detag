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

## Monitoring

- Grafana dashboards (as code): API goldens (latency, error rate), queue depth/age, job stage durations, success rate, quality gate pass rate, storage usage, redis memory, backup freshness.
- Alerts: readyz fail > 2 min, queue age p95 > 60 s, job failure > 5%/15 min, disk > 80%, redis down, backup older than 26 h.
- Nightly reconciliation job writes `system` summary to `09-STATE/PROGRESS.md` when running in the dev repo context.
