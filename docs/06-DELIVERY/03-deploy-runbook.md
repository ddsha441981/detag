# Deployment Runbook

## First deploy (single VPS)

1. Provision Ubuntu LTS, 4 vCPU / 8 GB / 100 GB SSD, Docker + compose plugin installed.
2. Clone repo, create `.env` from `.env.example`:
   - Required: `PF_JWT_SECRET` (64B random), `PF_DATABASE_URL`, `PF_REDIS_URL`, `PF_S3_*` (or bundled MinIO), `PF_PUBLIC_BASE_URL`, `PF_DOMAIN` (the public domain caddy serves; localhost = plain HTTP), `PF_STRIPE_SECRET_KEY` (+ webhook secret), SMTP vars.
   - Feature flags default OFF (razorpay, gpu, provenance_strip).
3. `docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env up -d --wait` → `just migrate deploy` → create admin: `docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod exec api pf-admin-create --email you@example.com` (prints one-time setup link).
4. Caddy front: automatic HTTPS for the domain; only caddy + web + api are on the `edge` network; db/redis/minio on `internal` only.
5. Verify: `/healthz` 200, `/readyz` 200, register test user, upload test image, job succeeds, quality report shows pass.
6. Backups: cron `ops/backup.sh` (pg_dump + minio mirror to off-host storage) nightly; verify restore doc step below.

### API image (P0-083)

`Dockerfile.api` is a multi-stage cargo-chef build (pinned
`lukemathwalker/cargo-chef:0.1.78-rust-1.99.1-slim-bookworm`, matching
`rust-toolchain.toml`) into a `debian:bookworm-slim` runtime: non-root
user `detag` (uid 10001), `curl` + `ca-certificates` only (the binary
links just glibc/libm/libgcc; the S3 client needs the CA bundle for
TLS), `HEALTHCHECK` on `/healthz` (default port 8080). Build it with
`docker build -f Dockerfile.api -t detag-api:<tag> .` — the build
context excludes `target/` (100+ GB) via `.dockerignore`. NFR-63
caps API images at 150 MB compressed, enforced by `just
image-size-check` (CI `image-size` job); the gate reads the
uncompressed size, a conservative proxy for the compressed limit.
Live verification is deferred to CI (DOCKER-001: no working docker
daemon on this VM).

### Worker image (P0-084)

`Dockerfile.worker` is the same cargo-chef base with the worker
binary, `ffmpeg` runtime deps (video work lands in Phase 3), and a
models volume: `PF_ENGINE_MODELS_DIR` defaults to `/models` inside
the image and compose mounts the volume there (P0-085). The worker
verifies every model the registry requires (sha256) at startup,
before consuming jobs — a failed verification is a boot error.
Phase 0 requires no model, so the check attests an empty registry;
downloading a missing model lands in Phase 2 with the first real
registry entry (license audit). `HEALTHCHECK` hits the worker's
`/metrics` on port 9092. Size: the same `just image-size-check`
builds this image against a 200 MB uncompressed ceiling — a Phase-0
ops guardrail, not an NFR (NFR-63 caps API images only; the worker
ceiling comes from the parts estimate, ~146 MB).

### Compose stack (P0-085)

`ops/compose.base.yml` is the shared stack: `edge` (caddy + web +
api) and `internal` (api + worker + postgres + redis + minio, no
route out) networks; `pgdata`, `miniadata` and `models` volumes; the
api, worker, web placeholder (dev-only profile), postgres, redis,
minio and the one-shot minio bootstrap. The dev/prod overrides
(`compose.dev.yml` P0-086, `compose.prod.yml` P0-087) layer on top:
`docker compose -f ops/compose.base.yml -f ops/compose.dev.yml
--env-file .env up -d --wait`. Two things P0-085 had to fix for
compose to work at all: the api and the metrics servers bind
`PF_API_HOST` / `PF_METRICS_HOST` (default 127.0.0.1; compose sets
0.0.0.0 — inside a container 127.0.0.1 is the container's own
loopback, unreachable from other containers or published ports),
and inter-service URLs use service DNS names (`postgres`, `redis`,
`minio`), never localhost. Validate with `just compose-base-check`
(renders for no profile, dev and prod; asserts web is dev-only).
The per-service fragments (`compose.{db,redis,minio}.yml`) stay as
the VM-local host-networking path (ADR-0014). Env wiring is gated
by `just compose-env-check` (P0-089): every `${VAR}` in every
`ops/*.yml` must be a `.env.example` key with a matching default,
every secret-named env value must be a `${...}` interpolation (no
literal secrets in yml), and `$${VAR}` shell escapes must name a
var the same service sets. One documented exception: the
bootstrap's `PF_S3_ENDPOINT` defaults to `http://minio:9000` (the
service DNS name) while `.env.example` defaults to
`http://localhost:9000` (the host-side default) — container vs
host, both correct in their runtime. Live `up` waits for
CI/E0.9's smoke (P0-090, DOCKER-001).

### Prod stack (P0-087)

`ops/compose.prod.yml` layers over the base (never alone — it only
overrides base services, plus caddy):

```sh
docker compose -f ops/compose.base.yml -f ops/compose.prod.yml --profile prod --env-file .env up -d --wait
```

- **Caddy edge (auto-TLS):** `caddy:2.11-alpine` (pinned, verified
  via the Hub API) listens on :80/:443 and is the only entry point —
  it reverse-proxies everything to the api (`ops/caddy/Caddyfile`).
  `PF_DOMAIN` (default `localhost`) is the domain caddy serves:
  localhost stays plain HTTP; a public DNS name gets a certificate
  provisioned automatically. Set `PF_PUBLIC_BASE_URL` to match
  (`https://<domain>`).
- **Internal isolation:** nothing but :80/:443 is published — the
  api port is unpublished in prod (dev publishes :8080 directly,
  P0-086), postgres/redis/minio stay on the `internal` network, the
  MinIO console is not published, and `PF_ENVIRONMENT: prod` means
  `/docs` is never served (P0-067). The console lands in Phase 4;
  until then there is no web service in prod.
- **Resource limits (mem caps):** sized for the 4 vCPU / 8 GB VPS
  above — api 1g, worker 2g (engine models), postgres 1g, redis
  256m (matches its maxmemory cap), minio 1g, caddy 256m; ~5.5g
  total, leaving headroom for the host.
- **Restart policies:** `unless-stopped` for every long-running
  service (the one-shot minio bootstrap keeps `restart: "no"`).
- **Log rotation:** json-file, `10m` x 3 files per service (30 MB
  max). Backups (pg_dump + minio mirror cron) are a runbook
  procedure, not a compose service — see step 6 above.
- Validate with `just compose-prod-check` (renders base+prod;
  asserts caddy present, web absent, the api port unpublished,
  `PF_ENVIRONMENT: prod`, and the worker's 2g cap). Live `up` waits
  for CI/E0.9's smoke (P0-090, DOCKER-001).

### Healthchecks (P0-088)

Every service except the one-shot bootstrap has a healthcheck, and
`docker compose up -d --wait` blocks until all of them are healthy.
`just smoke` (P0-090) uses `--wait-timeout 120` — it fails if any
service is still unhealthy after 120 s. Each check's worst case to
healthy (start period + interval x retries) is inside that budget;
`just compose-health-check` enforces all of this statically (it
renders both profiles and reads the api/worker checks out of the
Dockerfiles, because `docker compose config` cannot see an image
HEALTHCHECK until the image is built).

| Service | Check | What it proves |
| --- | --- | --- |
| postgres | `pg_isready` (5s x 10, start 10s) | the database accepts connections |
| redis | `redis-cli ping` (5s x 10, start 10s) | the cache answers |
| minio | `curl -f /minio/health/live` (5s x 10, start 10s) | object storage is live |
| api | image HEALTHCHECK on `/healthz` (10s x 3, start 10s) | the api serves (see "Health endpoints" below) |
| worker | image HEALTHCHECK on `/metrics` :9092 (10s x 3, start 10s) | the worker process + metrics server are up |
| web (dev) | `wget --spider /` (5s x 10, start 5s) | the stub page answers 200 |
| mailhog (dev) | `wget --spider :8025/api/v2/messages` (5s x 10, start 10s) | the fake inbox API answers |
| caddy (prod) | `curl -f /healthz` through the proxy (5s x 10, start 10s) | the whole edge path works — a healthy caddy means caddy + api both up; if the api is down, caddy reports unhealthy too and `docker compose ps` shows the api unhealthy alongside it |

Startup ordering (`depends_on`): the api and worker wait for
postgres/redis/minio `service_healthy` AND for the bootstrap
`service_completed_successfully` — the bucket must exist before the
api serves (`/readyz` reports storage down until it does) or the
worker consumes jobs; a failed bootstrap blocks the api loudly
instead of serving with a missing bucket. Caddy waits for the api
`service_healthy`. The bootstrap itself is one-shot (`restart:
"no"`) and has no healthcheck by design — its completion is the
signal. Live `up --wait` waits for CI/E0.9's smoke (P0-090,
DOCKER-001).

### Smoke (P0-090)

`just smoke` is the end-to-end proof the stack works: it brings up
the dev stack (`docker compose up -d --wait --wait-timeout 120` —
compose itself fails when any service is still unhealthy after
120 s, P0-088), applies migrations, waits for `/healthz` 200 and
`/readyz` 200 (60 s each), enqueues a ping job with `just
smoke-ping` (P0-082) and asserts the worker processes it, then
prints the stack URLs. It runs in CI as its own `smoke` job (no
service containers — compose publishes :5432/:6379 itself). Exit 0
only when every step passed; on failure it prints `docker compose
ps` + the api/worker logs so the diagnosis is in the CI log.

- `just smoke --skip-up` — skip the compose up + migrate and
  prove the wait/ping/URL legs against an already-up stack (a
  previous up, or the native stack from `just dev`). On a partial
  native stack `/readyz` may report storage/models down (no MinIO
  on this VM) — the script then fails only when db or redis is
  down, and warns about the rest.
- `just smoke --down` — tear the compose stack down at the end.
- This VM cannot run the full smoke: containers cannot start here
  (DOCKER-001), so the `up` step fails loudly and only
  `--skip-up` is provable locally.

When smoke fails, diagnose in this order: (1) `docker compose ps`
— which service is unhealthy; (2) that service's logs (`docker
compose logs api` / `worker` / `postgres` / ...); (3) `curl
localhost:8080/readyz` — the per-component table names the down
dependency and its latency; (4) if the ping leg failed but the
stack is healthy, `just smoke-ping` on its own prints the observed
redis/worker state on stderr.

### Dev loop (P0-086)

`just dev` is the entrypoint: compose stack (base+dev) when docker
runs, native postgres/redis + native api/worker binaries on this VM
(DOCKER-001), then migrate + seed, then the URLs. Hot reload runs
on the HOST, not in a container — neither bacon nor cargo-watch is
installed on this VM; install one of them to get it:

```bash
cargo install bacon --locked            # or: cargo install cargo-watch --locked
bacon test                               # re-runs the test suite on every save
cargo watch -x 'test --workspace'        # same loop with cargo-watch
```

Rules of the loop: the watcher rebuilds and re-runs tests on the
host; the compose stack is untouched (its api/worker are release
images — rebuild them with `docker compose build` after a change
that must be reflected in a container). Seeded data comes from
`just seed` (idempotent — run it twice, the second run changes
nothing). Dev URLs: api :8080, `/docs` Swagger UI, web placeholder
:3000, MinIO console :9001, MailHog UI :8025, metrics api :9090 and
worker :9092. Mail sent by the api in dev lands in MailHog — never
in a real inbox.

## Environment variables (PF_*)

| Var | Used by | Default (dev) | Notes |
| --- | --- | --- | --- |
| `PF_DATABASE_URL` | api, worker | postgres://pf:pf@localhost:5432/pf | |
| `PF_POSTGRES_USER/PASSWORD/DB` | postgres (compose) | pf / pf / pf | dev credentials for the compose postgres service (P0-034); must match `PF_DATABASE_URL` |
| `PF_REDIS_URL` | api, worker | redis://localhost:6379 | |
| `PF_S3_ENDPOINT`, `PF_S3_BUCKET`, `PF_S3_ACCESS_KEY`, `PF_S3_SECRET_KEY`, `PF_S3_REGION` | storage | MinIO localhost:9000 | private bucket required |
| `PF_JWT_SECRET` | api | dev-fixed | 64B in prod; rotation = dual-verify window |
| `PF_ARGON2_M_COST_KIB`, `PF_ARGON2_T_COST`, `PF_ARGON2_P_COST` | api | 19456 / 2 / 1 | Argon2id password-hashing costs, OWASP first choice (P1-001); m 4096..=1048576 KiB, t/p 1..=16 |
| `PF_TOS_CURRENT_VERSION` | api | v1 | ToS version a registration must accept (P1-001); any other `accept_tos_version` is a 422 |
| `PF_DISPOSABLE_EMAIL_DOMAINS` | api | mailinator.com, guerrillamail.com, 10minutemail.com, tempmail.com, yopmail.com | comma-separated disposable domains rejected at registration (P1-001); empty = no denylist |
| `PF_RATE_LIMIT_IP_BURST`, `PF_RATE_LIMIT_IP_REFILL_PER_MIN`, `PF_RATE_LIMIT_ROUTE_BURST`, `PF_RATE_LIMIT_ROUTE_REFILL_PER_MIN` | api | 120 / 120 / 600 / 600 | token-bucket caps, per-IP + per-route (P0-103); refill 0 = never refills |
| `PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST`, `PF_RATE_LIMIT_LOGIN_ACCOUNT_REFILL_PER_MIN` | api | 10 / 1 | per-account login-attempt bucket (P1-003): burst 10, then 1/min; keyed by the sha256 of the email; P1-004 lockout is the durable backstop |
| `PF_RATE_LIMIT_LOGIN_IP_BURST`, `PF_RATE_LIMIT_LOGIN_IP_REFILL_PER_MIN` | api | 20 / 10 | per-IP login-attempt bucket (P1-020): burst 20, then 10/min, across all accounts from one IP — bounds credential-stuffing runs; keyed by the sha256 of the client IP |
| `PF_RATE_LIMIT_REGISTER_EMAIL_BURST`, `PF_RATE_LIMIT_REGISTER_EMAIL_REFILL_PER_MIN` | api | 5 / 1 | per-email registration bucket (P1-020): burst 5, then 1/min; keyed by the sha256 of the email; duplicate attempts spend it too |
| `PF_RATE_LIMIT_RESET_ACCOUNT_BURST`, `PF_RATE_LIMIT_RESET_ACCOUNT_REFILL_PER_MIN` | api | 10 / 1 | per-account password-reset bucket (P1-020): the bucket exists for the reset flow (P1-025); no reset endpoint exists yet, so nothing spends from it today |
| `PF_PUBLIC_BASE_URL` | api | http://localhost:8080 | used in signed URLs |
| `PF_DOMAIN` | caddy (compose) | localhost | public domain caddy serves (P0-087): localhost = plain HTTP; a public DNS name gets auto-HTTPS; prod sets the real domain and PF_PUBLIC_BASE_URL to match |
| `PF_API_PORT` | api | 8080 | port the api binds on PF_API_HOST |
| `PF_API_HOST` | api | 127.0.0.1 | IP the api binds on (P0-085): loopback natively; compose sets 0.0.0.0 — inside a container 127.0.0.1 is the container's own loopback, unreachable from other containers or published ports |
| `PF_METRICS_HOST` | api, worker | 127.0.0.1 | IP the internal /metrics servers bind on (P0-085): same rule as PF_API_HOST |
| `PF_METRICS_PORT` | api | 9090 | port the internal /metrics server binds on PF_METRICS_HOST |
| `PF_ENVIRONMENT` | all | dev | dev \| stage \| prod; gates the Swagger UI (`/docs`, P0-067) — prod serves no `/docs` |
| `PF_WORKER_CONCURRENCY` | worker | 4 | apalis workers per queue (pipeline, cleanup, email, reconciliation — P0-075); must be >= 1 |
| `PF_WORKER_METRICS_PORT` | worker | 9092 | port the worker's internal /metrics server binds on 127.0.0.1 (P0-080) |
| `PF_WORKER_HEARTBEAT_INTERVAL_SECS` | worker | 15 | how often the worker writes its heartbeat row (P0-080): pid, queues, in-flight count, started_at; must be >= 1 |
| `PF_WORKER_DRAIN_TIMEOUT_SECS` | worker | 30 | graceful-drain bound (NFR-21, P0-081): after SIGTERM/SIGINT the worker finishes in-flight jobs for at most this long, then exits 1; un-acked jobs re-queue via the visibility timeout |
| `PF_WORKER_QUEUE_NAMESPACE` | worker | pf:queue | Redis key namespace for the queues (P0-081): `pf:queue:{queue}`; point a test/staging worker at its own namespace so it never reads production jobs |
| `PF_QUEUE_VISIBILITY_TIMEOUT_SECS` | worker | 300 | visibility timeout (NFR-21, P0-079): a dead worker's in-flight jobs are re-queued after this long without a keep-alive; must be > keep-alive |
| `PF_QUEUE_KEEP_ALIVE_SECS` | worker | 30 | worker heartbeat interval (P0-079): a live worker re-registers its consumer this often, so a long job on a live worker is never re-queued; must be >= 1 and < visibility |
| `PF_STRIPE_SECRET_KEY`, `PF_STRIPE_WEBHOOK_SECRET` | billing | test keys | live keys = owner action |
| `PF_RAZORPAY_KEY`, `PF_RAZORPAY_SECRET`, `PF_FEATURE_RAZORPAY` | billing | off | |
| `PF_SMTP_HOST`, `PF_SMTP_PORT`, `PF_SMTP_USER`, `PF_SMTP_PASS`, `PF_MAIL_FROM` | mailer | MailHog | |
| `PF_ENGINE_MODELS_DIR`, `PF_ENGINE_GPU`, `PF_ENGINE_TILES_MAX` | worker/engine | ./models, off, 1024 | models auto-download + sha256 verify |
| `PF_RESULTS_TTL_FREE/PRO/BUSINESS_H` | cleanup | 6/24/72 | plan TTLs |
| `PF_UPLOAD_PART_URL_TTL_SECS` | storage | 600 | presigned PUT part URL lifetime; must stay under the 15-min access-token TTL (P0-055) |
| `PF_RATELIMIT_*` | api | per-plan defaults | tuned Phase 5 |
| `PF_FEATURE_PROVENANCE_STRIP` | api | off | legal gate required |
| `PF_LOG_LEVEL`, `PF_OTEL_ENDPOINT` | all | info, empty | OTLP/HTTP; `/v1/traces` appended when the endpoint has no path |
| `PF_TEST_LIVE_MINIO` | tests | (empty = skip) | live-MinIO leg of the storage integration suite (P0-061); `1` runs it against testcontainers MinIO in CI |
| `PF_TEST_CONTAINERS` | tests | (empty = skip) | testcontainers leg of the api integration suite (harness.rs); `1` runs it against real postgres/redis containers in CI |
| `PF_WEB_ORIGIN` | api | (empty = CORS off) | the single browser origin allowed cross-origin (P0-065); empty disables CORS entirely; dev console: `http://localhost:3000` |

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
3. If queue stuck: `just queue::requeue-stuck` (visibility-timeout sweep — see "Visibility timeout (NFR-21)" below; the ticket spells it with a single colon, just only accepts the double-colon form).

## Secret rotation (P0-105, drafted)

Status: DRAFTED, not rehearsed. The procedures below are the
intended ones; the Phase 5 hardening ticket rehearses them on
staging with evidence ("JWT dual-key window procedure,
stripe/smtp/s3 key rotation"). Until the rehearsal, treat any
step that touches a live credential as owner-supervised.

Principles:

- A leaked credential rotates IMMEDIATELY (not on the next
  schedule): rotate first, file an S1 issue, then purge history
  only with owner approval (see `.gitleaks.toml` + CONTRIBUTING
  "Secrets policy").
- Rotation never means downtime for the api: the new credential
  is introduced alongside the old one, traffic moves to it, and
  only then is the old one revoked. Never delete the old
  credential before the new one is proven live.
- Secrets live only in the deployment `.env` (never committed,
  never in logs). Prod must set a real 64-byte `PF_JWT_SECRET`
  — the dev default (`dev-fixed`) must never reach prod. Known
  gap (P0-105): the api does NOT fail fast in prod on the
  placeholder default — the runbook checklist is the only guard
  today; a boot-time guard is a Phase 5 hardening candidate.

### JWT — dual-verify window

DRAFTED procedure; the code does NOT exist yet. Bearer auth
lands in Phase 1 (P1-016) — today `auth.rs` only classifies
tokens. The procedure below is what P1-016 implements, and
Phase 5 rehearses:

1. Generate a new 64-byte random secret.
2. Deploy it as the SECONDARY secret: the verifier accepts
   tokens signed by either the current or the secondary secret,
   but new tokens are still signed with the current one.
3. Promote: new tokens are signed with the new secret; the old
   secret stays accepted for verification.
4. Wait out the window — longer than the maximum token TTL, so
   every token signed with the old secret has expired.
5. Retire: remove the old secret from the verifier, deploy,
   verify that a token signed with the old secret is rejected
   and a fresh login works.

Session impact: JWTs are stateless, so there is no server-side
revocation list — a token signed with the old secret keeps
working until the window ends (step 4), then fails and the
client re-logs-in. The exact secondary-secret env var name is
set by P1-016's implementation; this section names the
procedure, not the variable.

### S3 keys (MinIO, bundled)

The app's `PF_S3_ACCESS_KEY` / `PF_S3_SECRET_KEY` are the
MinIO root credentials in the bundled setup (base compose maps
them to `MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD`):

1. Generate a new access-key / secret pair.
2. Update `.env` on the host (never commit, never paste into a
   ticket).
3. Restart MinIO, then rolling-restart api + worker.
4. Verify end to end: an upload completes, a job runs, a
   result URL renders.
5. Verify the OLD pair no longer authenticates (`mc alias set`
   with the old secret must fail), then consider the rotation
   done.

Caveat for the rehearsal: MinIO reads the root credentials
from the environment at boot, but on an EXISTING data
directory the stored root credentials may win over the new
env values — the Phase 5 rehearsal must confirm the behavior
for the deployed MinIO version. If the new pair does not take,
the fallback is a dedicated app user (`mc admin user add`
with a bucket-scoped policy) instead of the root pair, and
the api/worker point at that user from then on.

### Postgres password

1. `ALTER ROLE pf WITH PASSWORD '<new>';` on the live database.
2. Update `PF_POSTGRES_PASSWORD` and `PF_DATABASE_URL` in
   `.env` (they must match — the URL is built from the same
   vars).
3. Rolling-restart api + worker; verify `/readyz` reports the
   database component healthy.
4. The old password stops working at step 1 — there is no
   dual-password window for Postgres, so do this in a low-
   traffic window.

### Payment keys (Stripe / Razorpay)

Rotate in the provider dashboard (new secret key, new webhook
signing secret), update `.env`, restart the api, verify a test
checkout. The webhook endpoint secret must be rotated on BOTH
sides: the provider's webhook endpoint settings and
`PF_STRIPE_WEBHOOK_SECRET` — a mismatch fails signature
verification and the webhook is rejected (safe, but billing
events stop flowing until they match).

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
  scrape job (E0.5, P0-044+); `/readyz` itself exists since P0-066
  (db/redis/storage/models, per-component latency) — see
  "Health endpoints" under Observability. Until the exporter
  lands, Redis health is the compose healthcheck plus
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

### Visibility timeout (NFR-21, P0-079)

Kill -9 of a worker mid-job SHALL NOT lose the job: it
re-enters the queue via the visibility timeout. How it works
(apalis-redis 0.6): when a worker fetches a job, the job moves
from the queue's `active` list to that worker's in-flight set
(`pf:queue:{queue}:inflight:{worker}`), and the worker
registers itself as a consumer (`pf:queue:{queue}:consumers`,
score = last keep-alive timestamp). Every live worker runs an
orphan sweep every poll interval (100 ms): any consumer whose
last keep-alive is older than the visibility timeout has its
in-flight jobs moved back to the active list.

- `PF_QUEUE_KEEP_ALIVE_SECS` (default 30): the heartbeat. A
  live worker re-registers this often — so a job that runs
  longer than the visibility timeout on a LIVE worker is never
  re-queued. Only a dead worker's jobs become orphaned.
- `PF_QUEUE_VISIBILITY_TIMEOUT_SECS` (default 300): how long
  after a worker's last keep-alive its jobs count as orphaned.
  A kill -9 mid-job returns the job to the queue at most this
  long after the worker died. Must be greater than the
  keep-alive (boot fails otherwise — a worker merely slow to
  heartbeat must not have its jobs stolen mid-run). Raising it
  delays recovery after a crash; lowering it risks re-running
  a job whose worker is alive but slow — 300 s is the right
  default until jobs run longer than 5 minutes (Phase 3 video
  work revisits it).
- If EVERY worker is dead at once, no heartbeat runs and no
  sweep happens — restart a worker, or re-queue immediately
  without one: `just queue::requeue-stuck` (same sweep, run on
  demand; `--queue <name>` for one queue, `--older-than-secs N`
  to override the timeout, `--limit N` per queue, default 100 —
  re-run to drain a larger backlog). It never touches a live
  worker's jobs. The kill-mid-job drill that proves this end
  to end lands in Phase 3; the sweep itself is covered by the
  `requeue_stuck` integration test.

## Storage operations

The object store holds every byte that is not in Postgres:
uploads (inputs), job artifacts (outputs, masks, previews,
reports, batch zips). One bucket per environment, named
`detag-<env>` — dev is `detag-dev` today (`.env.example`,
`PF_S3_BUCKET`). The bucket is always private: objects are
reached only through presigned URLs (P0-055), never through a
public policy — `scripts/check_minio_policy.py` fails CI if
that ever changes. Retention lives in the next section
("Storage lifecycle & retention"); this section is the
day-to-day operating half.

### Buckets

- **Dev:** MinIO in `ops/compose.minio.yml` (`minio/minio:latest`,
  host networking per ADR-0014, named volume `detag_miniodata`
  for `/data`). The bootstrap service creates the bucket and
  locks it private (`mc mb --ignore-existing`, then
  `mc anonymous set none`) and applies the lifecycle JSON.
  `docker compose -f ops/compose.minio.yml up -d --wait`.
- **Staging / prod:** the bucket must already exist before the
  api/worker start (bucket creation is an ops concern — the
  `S3Storage` client never creates one). Create it with the
  provider console or CLI, private, with the lifecycle JSON from
  `ops/minio/lifecycle.json` applied (see the next section).
- **Pointing at R2 / S3 (endpoint override):** only the env
  changes — the code is backend-agnostic (`S3Storage`, one code
  path, `force_path_style(true)`):
  - R2: `PF_S3_ENDPOINT=https://<account-id>.r2.cloudflarestorage.com`,
    `PF_S3_REGION=auto` (R2 ignores region for signing, but the
    field must be non-empty), `PF_S3_ACCESS_KEY` /
    `PF_S3_SECRET_KEY` are an R2 API token (Object Read & Write
    on the bucket — never an admin token on the server).
  - S3: `PF_S3_ENDPOINT=https://s3.<region>.amazonaws.com`,
    `PF_S3_REGION=<region>`, keys are an IAM user/role with
    `s3:Get/Put/DeleteObject` + `ListBucket` on the bucket only.
  - The client forces path-style URLs, which MinIO, R2 and S3
    all accept today. If S3 ever drops path-style support, the
    fix is one flag in `S3Storage::new` — not a redesign.
- **Never** point two environments at one bucket, and never
  point dev at a prod bucket: keys carry only `{env}/{yyyy-mm}/`
  (ADR-0011), so a shared bucket would mix environments in one
  listing and one lifecycle rule.

### Keys

Layout (ADR-0011, P0-056): `{env}/{yyyy-mm}/{job_id}/{stem}.{ext}`
— e.g. `prod/2026-11/019aa1b2-…/output.png`. No PII by
construction: the environment is `dev`/`staging`/`prod`, the
date is a `NaiveDate`, the job id a UUID, the stem one of the
seven `ArtifactKind` values (`input` plus the six
`artifacts.type` values: `output`, `mask`, `preview_before`,
`preview_after`, `report`, `batch_zip`); the original filename
lives in the `uploads` row, never in the key.
`crates/storage/src/keys.rs` builds and parses keys
(`parse_storage_key` re-validates every segment on read-back),
and `crates/core/src/checksum.rs` + `media.rs` own the
verify-on-complete steps (sha256 per part, PF-2004 on mismatch;
magic-byte sniff, PF-2001 on unknown type).

### Presigned flows (operator view)

Part PUT URLs live 600 s, the token 900 s, result GET URLs
6 h / 24 h / 72 h by plan (Free / Pro / Business) — the numbers
live in `crates/storage/src/policy.rs` (`PresignPolicy`) and
`PF_UPLOAD_PART_URL_TTL_SECS`. SigV4 URLs are single-use in
no sense at all (`SINGLE_USE_SUPPORTED = false`): the
compensating controls are the short TTLs, one key per URL, and
upload ids dying after complete/abort. If a URL leaks, it dies
with its TTL — there is nothing to revoke; rotate the bucket
credentials only if the keys themselves leak.

### Local console (MinIO, dev only)

`http://localhost:9001` — log in with `PF_S3_ACCESS_KEY` /
`PF_S3_SECRET_KEY` (`minioadmin` / `minioadmin` in dev). Use it
to eyeball the bucket: object list, a single object's metadata,
and the bucket's anonymous policy (must read `none`). The
console is dev-only: the prod compose file (E0.9) must not
expose it. Prefer `mc` for anything repeatable:

```bash
mc alias set detag "$PF_S3_ENDPOINT" "$PF_S3_ACCESS_KEY" "$PF_S3_SECRET_KEY"
mc ls detag/"$PF_S3_BUCKET"                 # top level (env prefixes)
mc du detag/"$PF_S3_BUCKET"                 # size by prefix
mc ilm list detag/"$PF_S3_BUCKET"           # the applied lifecycle rule
mc anonymous get detag/"$PF_S3_BUCKET"      # must be `none`
```

### Cost note

The retention design keeps stored bytes tiny on purpose —
results die after 6–72 h, inputs after 3 days at most — so the
bill is dominated by requests and egress, not storage:

- **MinIO (dev):** $0 beyond the disk it sits on. Watch the
  named volume: MinIO refuses writes when the disk fills, so
  `mc du` + host disk alerts are the monitoring, and growing
  the volume is the fix.
- **R2:** Standard $0.015/GB-month, Infrequent Access
  $0.01/GB-month, free tier 10 GB-month + 1M Class A + 10M
  Class B ops/month, and **zero egress fees** (figures from
  Cloudflare's R2 docs/pricing, checked 2026-11-12). For this
  workload — short-lived objects, downloads as the product —
  zero egress is the reason R2 is the default prod candidate.
- **S3:** Standard ≈ $0.023/GB-month in us-east-1, requests
  ≈ $0.0004–$0.005 per 1,000 depending on class, egress
  $0.09/GB after the first 100 GB/month free (verify at deploy
  — AWS pricing moves). Egress is the number to watch: every
  result download is egress, and a viral job's output is
  downloaded far more than it is stored.
- **What would change the bill:** keeping inputs past 3 days
  (don't — the ILM ceiling forbids it), storing previews at
  full resolution (they're derived artifacts, same TTL), or a
  GC outage letting orphans accumulate (the GC's dry-run count
  is the canary — see the next section).

### When storage is down

Design rule (enforcement lands with the API error mapping,
Phase 2/3): the api/worker must fail closed on a down store —
no silent local fallback, no writing uploads to disk. Check, in
order:
`docker compose -f ops/compose.minio.yml ps` (or the MinIO
health endpoint `http://localhost:9000/minio/health/live`),
`mc alias set` + `mc ls` (credentials vs connectivity),
disk space on the MinIO host, then the api logs for the
`StorageError` source (the PII-free Display names the kind;
the source has the backend's message). Never "fix" a down
store by pointing the app at another bucket's data — restore
the store.

## Storage lifecycle & retention

Two layers, because neither alone can express the retention
promise (FR-J-09: results live per plan — Free 6 h, Pro 24 h,
Business 72 h — then are deleted):

| Layer | What it does | Granularity |
| --- | --- | --- |
| ILM ceiling (`ops/minio/lifecycle.json`) | Deletes **everything** in the bucket after 3 days | Days (the backend's limit) |
| `detag-gc` job (`apps/gc`, `just gc`) | Deletes expired **artifacts** from `artifacts.expires_at` + orphans older than 24 h | Exact (per-row `expires_at`) |

- **Why two layers:** S3, MinIO and R2 lifecycle rules all work
  in whole days — a 6 h or 24 h TTL cannot be expressed in ILM
  at all. The GC enforces the per-plan TTLs from the database
  (each artifact row carries `expires_at = created + plan TTL`);
  the ILM rule is the backstop that guarantees nothing — inputs
  included — survives past 72 h, the longest TTL.
- **Inputs vs outputs:** the GC never deletes an input (uploads
  have no `expires_at`); the ILM 3-day ceiling is the input
  backstop. This is deliberate: keeping a user's original past
  its result's TTL would be a privacy liability, and the key
  layout (ADR-0011) has no per-kind prefix for ILM to scope by,
  so the ceiling is bucket-wide on purpose.
- **Orphans:** an object no database row references (a failed or
  abandoned upload's parts, a crashed job's output). The GC
  deletes orphans older than 24 h (`--orphan-grace-hours`); the
  grace is belt-and-braces — an upload's row is created before
  its presigned PUT, so a live upload is never an orphan.
- **MinIO (dev):** the compose bootstrap applies the JSON with
  `mc ilm import` after creating the bucket and locking it
  private. `scripts/check_minio_lifecycle.py` (run by
  `just compose-minio-check`) fails CI if the rule is disabled,
  scoped to a prefix, or expires past 3 days. The JSON also aborts
  incomplete multipart uploads after 7 days (R2's built-in
  default; MinIO/S3 need it explicit).
- **S3 (prod):** the same document, via
  `aws s3api put-bucket-lifecycle-configuration --bucket <name> --lifecycle-configuration file://ops/minio/lifecycle.json`.
- **R2 (prod alt):** the same 3-day expiry, via the dashboard
  (bucket → Settings → Object Lifecycle Rules) or
  `npx wrangler r2 bucket lifecycle add <bucket> detag-results-ceiling-72h --expire-days 3`
  (R2 also accepts the S3 lifecycle API). R2's default rule
  already aborts incomplete multipart uploads after 7 days.
- **Running the GC:** `just gc` dry-runs (lists, reports,
  deletes nothing). The deleting run is
  `cargo run -p detag-gc -- --execute`. It fails closed: if
  Postgres or S3 is unreachable it exits 1 before deleting
  anything (a run without the database's "known" set would
  mistake every object for an orphan). A failed delete is
  reported, not fatal — the next run retries it. Scheduling
  (a daily run) lands with the delivery work; today it is run
  by hand. The account purge is a separate mode of the same
  binary (P1-007): `cargo run -p detag-gc -- --purge-users`
  dry-runs the purge (lists past-grace accounts, deletes
  nothing); `--purge-users --execute` deletes them. It runs
  INSTEAD of the artifact sweep (one mode per invocation —
  the nightly cron calls it separately) and needs the database
  only (no S3 client: purged users' objects become orphans
  for the next sweep). The session purge is the same shape
  (P1-015): `cargo run -p detag-gc -- --purge-sessions`
  dry-runs (counts sessions past `expires_at`, deletes
  nothing); `--purge-sessions --execute` deletes them — also
  database-only, also instead of the sweep, also called
  separately by the nightly cron.
- **Live verification:** the ILM import and the GC's execute
  path wait for CI/E0.9 (DOCKER-001 — no containers on this VM).
  Until then the gates are the static checker, the repo live
  tests, and the GC's unit tests against the memory fake.

## Observability

### Health endpoints

`GET /healthz` (liveness — the process answers `"ok"`) and
`GET /readyz` (readiness — FR-X-02, P0-066) are unauthenticated
and unversioned by design: orchestrators and load balancers must
reach them without credentials. `/readyz` answers 200 when every
component is ok, 503 otherwise; the body is always the same JSON
(the status code is the signal, the body is the diagnosis):

```json
{ "status": "ok",
  "components": [
    { "name": "db", "ok": true, "detail": "ok", "latency_ms": 3 },
    { "name": "redis", "ok": true, "detail": "ok", "latency_ms": 1 },
    { "name": "storage", "ok": true, "detail": "ok", "latency_ms": 12 },
    { "name": "models", "ok": true, "detail": "no models registered — none required until Phase 2", "latency_ms": 0 } ] }
```

- **db:** `SELECT 1` against the pool. **redis:** TCP `PING` →
  `+PONG`. **storage:** `HeadBucket` on the configured bucket —
  a missing bucket, bad credentials, or an unreachable endpoint
  all report down. **models:** the engine registry state — in
  Phase 0 no model is required, so an empty registry is ok and
  the detail says so; Phase 2 replaces this with a real
  "required models present + verified" check (P0-084).
- Each probe is bounded by its own 2 s timeout and the four run
  concurrently, so a fully-down dependency set still answers in
  ~2 s. On a dead database the db probe takes the whole 2 s
  (sqlx retries the acquire internally — ASSUMPTION-012).
- Details never contain credentials or provider error text
  (`StorageError`'s display is PII-free by construction; db/redis
  details are redacted the same way).

```bash
curl -fsS http://localhost:8080/healthz   # ok
curl -s http://localhost:8080/readyz | python3 -m json.tool
```

Compose healthchecks consume these endpoints (P0-088 wires them
into `compose.base.yml`; the Dockerfile `HEALTHCHECK` lands with
P0-083). Until then, `just dev` reports `/readyz` after boot and
`just smoke` (P0-090) waits on both endpoints for every service.

### Smoke ping (P0-082)

`just smoke-ping` runs the `smoke-ping` bin
(`apps/worker/src/bin/smoke-ping.rs`): it checks the api's
`/healthz`, enqueues one `ping` envelope on the `pipeline` queue,
and waits until the worker's `/metrics` shows the dequeue count
rise and the in-flight gauge return to 0, then prints `smoke ping
done (job_id=...)` and exits 0. A non-zero exit names what was
observed (api down, metrics unreachable, or the ping not processed
within `--timeout-secs`, default 30 s). Run it against the native
stack with the api + worker up; `just smoke` (P0-090) calls the
same bin against the compose stack. Honest boundary: the probe
proves the redis → worker leg plus the api being up — it does not
prove the api enqueues, because `POST /v1/jobs` is a 501 stub
until job creation lands in Phase 1.

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
| **Detag queues** (`queue.json`) | Depth per queue, enqueue/dequeue rate, stage duration p50/p95, stage runs observed, worker in-flight jobs, heartbeat age, live workers | Depth near 0 between bursts; dequeue keeps up with enqueue; stage p95 stable; heartbeat age under ~30 s per worker |
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
| `DetagApiDown` (`up{job="detag-api"} == 0`, 2m) | page | Prometheus cannot scrape the api's `/metrics` — the api is down or its metrics port is blocked. Proxy for "readyz fail > 2 min"; there is still no Prometheus series scraping `/readyz` itself (a blackbox/readyz probe lands with the compose stack, E0.9). | `docker compose ps`; api logs; check the metrics port is reachable. `curl localhost:8080/readyz` reports db/redis/storage/models health directly (P0-066). |
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

Access log (P0-019, P0-072): every request gets a `request completed`
event only when it should be logged — always for errors (status >=
400) and slow requests (> 250 ms, the NFR-1 threshold), and for a
deterministic 1% of fast successes (a hash of the request id, so the
same id always decides the same way). The `http_request` span exists
for every request regardless of sampling — the id is always in the
logs, even when the access event is not.
The event line carries `request_id`, `method`, `route` (the matched
path template, `unmatched` for 404s), `status`, `elapsed_ms`, and
`sampled`: true only when the line exists because the 1% sample
picked the request, false for errors and slow requests. No query
string is ever logged.
Read the access log with: `docker compose logs api | grep 'request completed'`
(all logged requests); add `grep 'sampled=true'` for the 1% sample
only, or `grep 'status=5'` for server errors. A slow request is any
line with `elapsed_ms` over 250; the p99 for auth/jobs/status must
stay under 250 ms (NFR-1).

Log hygiene (P0-032): logs never contain emails, passwords, tokens,
or raw IPs — values are redacted (`[email]`, `[redacted]`) or
hashed (`[ip:…]`) before they are emitted. If you ever see a raw
one in a log line, that is a bug: file it, do not paste the line
into a ticket.

- Nightly reconciliation job writes `system` summary to `09-STATE/PROGRESS.md` when running in the dev repo context.
