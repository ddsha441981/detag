# ISSUES.md — bug & blocker backlog (agent-maintained)

> Schema per 06-DELIVERY/01-bug-triage.md. Severity first-class: S1 interrupts everything. Keep entries updated (`status: open | in_progress | fixed(<commit>) | wontfix(ADR)`). Tags: `blocked:`, `assumption:`, `needs-owner-ok:`, `flaky:`.

## Open issues

### ASSUMPTION-010 [assumption] Dev Redis/Postgres do NOT survive a VM reset — reinstall + manual start (P0-051)
- Date: 2026-11-12 · Ticket: P0-051
- Assumption: at run start both native services were gone entirely — no `redis-server`/`psql` binaries, no `/etc/redis`, no `/var/lib/postgresql`, ports 6379/5432 closed — although ASSUMPTION-007/008 recorded them as installed. Restored by `apt-get install -y redis-server postgresql-16` (apt works via the VM egress proxy), then `redis-server /etc/redis/redis.conf --daemonize yes --save "" --maxmemory 256mb --maxmemory-policy allkeys-lru`, `pg_ctlcluster 16 main start`, `CREATE ROLE pf LOGIN PASSWORD 'pf'` + `ALTER ROLE pf CREATEDB` + `CREATE DATABASE pf OWNER pf` (dev placeholders from `.env.example`). Baseline `just verify` then green modulo the known FLAKY-001. After any VM reset, re-run the install + start steps before trusting `just verify`; `just dev` still prefers compose when docker works.
- Status: open

### ASSUMPTION-009 [assumption] P0-051 runs its integration suite against native Redis, not testcontainers (P0-051)
- Date: 2026-11-12 · Ticket: P0-051
- Assumption: the ticket spec names testcontainers, but testcontainers cannot run on this VM — containers cannot start (DOCKER-001) and the crate is not a dev-dependency. The suite (`crates/core/tests/redis_integration.rs`) instead runs against the dev Redis the same way the P0-046..P0-050 live tests do (native redis-server on this VM per ASSUMPTION-008, compose redis where docker runs / in CI), and covers exactly the ticket's three items: primitives under concurrency (mixed 30-task workload on one pool), pubsub latency p95 under load (≤ 500 ms), and failover mapping to the typed PF-5003 error. Adding testcontainers as the CI mechanism is a follow-up for when docker works; it needs no test changes beyond the harness (the tests already take the URL from `PF_REDIS_URL`).
- Status: open

### FLAKY-001 [flaky] `sigterm_drains_in_flight_request_and_exits_zero` fails intermittently (P0-029)
- Date: 2026-11-12 · Ticket: P0-029
- Symptom: `apps/api/tests/shutdown.rs` fails with `Connection reset by peer` on `read response` (finished in 0.11 s); the next identical run passes (0.21 s). Seen: fail, fail, pass across three consecutive runs of the same binary.
- Cause (suspected): timing race in the test itself — SIGTERM is sent while the request is still partial, then the test completes it; if the server starts draining first, hyper closes the mid-headers connection before the final blank line arrives.
- Evidence it is not a P0-029 regression: the test touches only `detag-api` + shutdown code, which P0-029 did not change; identical code fails and passes across runs.
- Status: open — re-running `just verify` until green is the workaround; a fix (e.g. complete the request before signalling, or retry the drain assertion) belongs to a hardening ticket, not P0-029.
- Update 2026-11-12 (P0-035): the flake got much worse — 3/6 and 4/6 failures in isolation runs (still the 0.11 s vs 0.21 s split). Ruled OUT as a P0-035 regression by a stash test: pre-P0-035 code fails 4/6 identically. Suspected cause is now environmental load (dockerd + native postgres now run on the VM alongside the test), which narrows the drain race window. The workaround still works but may need several retries.

### FLAKY-002 [flaky] `slow_consumer_drops_safely` failed once, passed on retry (P0-060)
- Date: 2026-11-12 · Ticket: P0-060
- Symptom: `crates/core/tests/pubsub_live.rs:179` failed once during the P0-060 `just verify` run (attempt 1 red); attempt 2 green with no code change. Single occurrence so far — not yet a pattern.
- Cause (suspected): timing-sensitive Redis pubsub test under load, same family as FLAKY-001. P0-060 touches only storage/GC code and does not change pubsub.
- Status: open — watch: if it recurs, promote to a hardening ticket like FLAKY-001; until then re-running `just verify` is the workaround.

### ASSUMPTION-004 [assumption] Dashboards validated statically; "visible in Grafana" unverified until E0.9 (P0-030)
- Date: 2026-11-12 · Ticket: P0-030
- Assumption: the P0-030 AC "dashboards visible in grafana" cannot be verified today — no compose files exist yet (E0.9, P0-085..P0-090), no Grafana runs on this VM, and Grafana cannot be installed via Docker Hub pulls (ADR-0017). The ticket instead delivers schema-valid dashboard JSON + provisioning, validated by `scripts/validate_dashboards.py` (JSON, uids, panels, PromQL allow-list) and enforced in CI; live visibility is re-verified when the compose stack lands.
- Chosen default: job-outcome/storage/Redis/backup panels are omitted (not faked as "No data" queries) and listed as gaps in `ops/grafana/README.md`; they land with E0.8/E0.4/E0.5/Phase 2.
- Status: open

### ASSUMPTION-005 [assumption] Prometheus "targets up" unverified live until E0.9 (P0-031)
- Date: 2026-11-12 · Ticket: P0-031
- Assumption: the P0-031 AC "targets up" cannot be verified live today — no Prometheus runs on this VM and the compose stack lands in E0.9 (P0-085..P0-090). "Rules load" IS verified for real: promtool 3.5.0 (downloaded from the official GitHub release into `.scratch/tools/`, not vendored) reports the config valid, 1 rule file found, 2 rules found; plus the static validator in CI. The scrape targets are the api's /metrics (verified serving by P0-026's tests) and Prometheus itself; both are confirmed up when the stack lands.
- Status: open

### ASSUMPTION-001 [assumption] `just dev` is a partial stack until E0.9/E0.4 land (P0-012)
- Date: 2026-11-12 · Ticket: P0-012
- Assumption: `just dev` (scripts/dev.sh) starts what exists today (api + worker via `cargo run`, /healthz wait) and reports compose deps, /readyz, migrations and seed as pending with their ticket ids — it does not fake a full stack. The Phase 0 exit gate re-verifies `just dev` against the full stack once P0-036 (migrations), P0-043 (seed) and P0-085..P0-090 (compose) land.
- Chosen default: seed step is gated on `ops/seed.sql` existing; if P0-043 provides a different seed artifact, dev.sh's gate is updated in that ticket.
- Status: open

### ASSUMPTION-002 [assumption] P0-022 ships the flag resolver + traits + in-memory store/cache; sqlx repo + Redis impl land with E0.4/E0.8 (P0-022)
- Date: 2026-11-12 · Ticket: P0-022
- Assumption: the real sqlx `FlagStore` and Redis `FlagCache` cannot be built or verified in this ticket — no sqlx/redis client in the workspace, no local Postgres/Redis, no migration runner (P0-036), and Docker Hub pulls fail (ADR-0017), so a live DB-backed test is impossible today. P0-022 therefore delivers the behaviour the ACs pin down (flip visible within TTL; invalidation test green) behind traits: `FlagResolver` (hides the source), `FlagStore` (repo interface) + `InMemoryFlagStore`, `FlagCache` + `InMemoryFlagCache`, `CachedFlagResolver` (30 s TTL, invalidate on admin edit), plus the `feature_flags` table as `migrations/0001_feature_flags.sql` (P0-036's runner applies it). The in-memory cache is test/dev only — in-proc caching breaks NFR-10; production uses Redis behind the same trait. `rollout` is stored, not yet evaluated (percentage rollouts are future work).
- Status: open

### ASSUMPTION-003 [assumption] /metrics binds loopback; compose isolation lands with E0.9 (P0-026)
- Date: 2026-11-12 · Ticket: P0-026
- Assumption: the /metrics server binds 127.0.0.1:PF_METRICS_PORT (default 9090) on its own port, never on the public api port — that satisfies "not on edge network" at the code level (verified: /metrics on the api port 404s). Compose network isolation (internal network only, port not published) cannot land here: no compose files exist yet (E0.9, P0-085..P0-090). E0.9 may need to revisit the loopback bind: a Prometheus container on another host in the compose network cannot scrape a loopback-bound listener, so the bind may have to become 0.0.0.0 with isolation enforced by the (unpublished) compose network instead.
- Status: open

### ASSUMPTION-006 [assumption] P0-032 exercises auth-failure via the error taxonomy, not HTTP — no auth middleware exists yet in Phase 0 (P0-032)
- Date: 2026-11-12 · Ticket: P0-032
- Assumption: the ticket's "auth-failure path" is exercised through the real error path that exists today: `AppError::EmailNotVerified.to_problem(Profile::Production, …)` with PII caller detail — the only place caller detail reaches the logs (its trace-only `tracing::error!`). No HTTP auth middleware exists in Phase 0 (auth lands in Phase 1, E1.x), so there is no HTTP 401/403 to trigger. When auth lands, its failure handler must log through the same redacted path; an HTTP-level hygiene test then becomes possible and should be added with the auth tickets.
- Status: open

### DOCKER-001 [blocked] Docker containers cannot start on this VM — missing kernel capabilities (P0-034)
- Date: 2026-11-12 · Ticket: P0-034
- Blocker: dockerd now runs (daemon.json applied, vfs driver, crun 1.30.1 + wrapper, compose plugin v2.40.3), images load, `docker compose config` validates, volumes create — but every container start fails: no NET_ADMIN (loopback bring-up `SIOCSIFFLAGS: Operation not permitted`; `--network host` fails in dockerd's sandbox creation), no keyctl session-keyring joins, zero cgroup v2 controllers delegated, no system bus. Full chain in the ADR-0014 amendment. Effect: P0-034's two live ACs (healthy under compose, volume persists) cannot be verified here; live compose verification is deferred to CI (docker works there) and E0.9's smoke (P0-090). Dev fallback: native PostgreSQL 16 on :5432 (role/db `pf`); `just dev` tries compose first, falls back, and waits on pg_isready either way. Do not re-attempt the runtime fixes without new kernel capabilities.
- Amendment 2026-11-12 (P0-054): the VM reset that took out redis/postgres (ASSUMPTION-010) also removed Docker entirely — no `docker`/`dockerd` binary, no `/var/lib/docker`, service inactive. `just compose-db-check` / `just compose-redis-check` now fail here with `docker: not found` (exit 127), and the same applies to the new `just compose-minio-check`'s docker half. Reinstalling docker is NOT in scope for the storage tickets: containers could not start on this VM even when docker existed. Compose files are validated statically here (`scripts/check_minio_policy.py` for the MinIO policy; YAML parse for the rest) and live verification waits for CI/E0.9.
- Status: open

### ASSUMPTION-007 [assumption] Native postgres is the dev DB on this VM until docker can run containers (P0-034)
- Date: 2026-11-12 · Ticket: P0-034
- Assumption: sqlx/migration tickets (P0-035+) verify against native PostgreSQL 16 on 127.0.0.1:5432 (role `pf`, db `pf`, same credentials as `.env.example`'s `PF_DATABASE_URL`); the compose file stays the canonical service definition and E0.9 folds it into compose.base.yml unchanged in shape. If docker starts working on this VM, `just dev` already prefers compose — no code change needed, just re-verify.
- Status: open

### ASSUMPTION-008 [assumption] Native redis is the dev Redis on this VM until docker can run containers (P0-046)
- Date: 2026-11-12 · Ticket: P0-046
- Assumption: redis tickets (P0-046+) verify against native redis-server 7.0.15 on 127.0.0.1:6379 (installed via apt, started manually with `redis-server /etc/redis/redis.conf`; `/etc/redis/redis.conf` carries the dev cache-role flags — `save ""`, `maxmemory 256mb`, `maxmemory-policy allkeys-lru` — mirroring ops/compose.redis.yml, original backed up at `/etc/redis/redis.conf.detag-orig`). The compose file stays the canonical service definition and E0.9 folds it into compose.base.yml unchanged in shape. The redis-server systemd unit CANNOT start on this VM (fails 217/USER, "Failed to set up user namespacing" — same missing-capabilities family as DOCKER-001), so after a VM reboot redis must be started manually with that command until docker works.
- Status: open

## Closed issues (keep for history; prune to last 50)

(none yet)

## Trend notes

(appended weekly by agent: opens/closes by severity, flaky count, oldest open)
