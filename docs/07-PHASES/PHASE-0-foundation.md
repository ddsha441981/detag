# PHASE 0 — Foundation & Platform Baseline

> **Tickets: 108** · Types: feat 51 · chore 35 · test 12 · docs 10

**Goal:** A green platform baseline every later ticket assumes exists: cargo workspace, config/errors, observability, Postgres+sqlx, Redis, storage trait, API skeleton, worker skeleton, docker compose, test harness, security baseline.

**Context (read before starting):**
- Everything here is load-bearing: later phases assume compose, CI, harness and the storage/queue abstractions exist exactly as specced.
- Do not skip the test-harness epic to move faster - every phase leans on it.
- Config defaults come from 06-DELIVERY/03-deploy-runbook.md (PF_* vars).

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] `just dev` brings the full stack healthy (api, worker, web placeholder, pg, redis, minio, prometheus, grafana).
- [ ] `just verify` green locally AND in CI (fmt, clippy -D warnings, tests, compose smoke).
- [ ] `just smoke` enqueues + processes the ping job end-to-end.
- [ ] OpenAPI scaffold live with drift-gate CI; dashboards render; threat-model stub committed.
- [ ] Test harness + golden-media generator working; coverage reporting live (report-only until Phase 2).

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E0.1 Repo & Tooling | repo | 14 |
| E0.2 Config & Errors | core/config | 10 |
| E0.3 Observability | observability | 9 |
| E0.4 Postgres & sqlx | db | 12 |
| E0.5 Redis | redis | 7 |
| E0.6 Storage | storage | 10 |
| E0.7 API skeleton | api | 12 |
| E0.8 Worker skeleton | worker | 8 |
| E0.9 Docker & Compose | ops | 9 |
| E0.10 Test harness & fixtures | testing | 8 |
| E0.11 Security baseline | security | 9 |

---

## E0.1 — Repo & Tooling (area: repo)

- [x] **P0-001** — Bootstrap cargo workspace
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Create Cargo workspace: members apps/api, apps/worker, crates/{core,engine,storage,billing,mailer}; workspace release profile (lto=thin, codegen-units=1, strip=symbols); rust-toolchain.toml pins stable MSRV.
  - **AC:**
    - [x] cargo build --workspace passes
    - [x] clippy -D warnings clean on empty crates
    - [x] toolchain pinned and CI-ready

- [x] **P0-002** — Scaffold crate skeletons
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-001 · **Prio:** P1
  - **Spec:** Each member compiles: api has axum bootstrap stub, worker a tokio main, crates have lib.rs with module placeholders per 04-ENGINEERING/02-repo-layout.md.
  - **AC:**
    - [x] cargo run -p api prints startup line
    - [x] trivial cargo test passes per crate
    - [x] no clippy warnings

- [x] **P0-003** — Formatting + lint policy
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-002 · **Prio:** P1
  - **Spec:** rustfmt.toml (max_width 100), clippy config, `just fmt` / `just lint`; allow-attribute policy documented in coding standards (justification comment required).
  - **AC:**
    - [x] fmt + lint targets run clean
    - [x] policy documented

- [x] **P0-004** — Repo hygiene files
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-003 · **Prio:** P1
  - **Spec:** .gitignore, .editorconfig, LICENSE (Apache-2.0; owner may veto via DECISIONS.md), CONTRIBUTING.md pointing at this kit, PR template with DoD checklist from 04-ENGINEERING/03-definition-of-done.md.
  - **AC:**
    - [x] files present
    - [x] PR template lists DoD items

- [x] **P0-005** — justfile task runner
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-004 · **Prio:** P1
  - **Spec:** Targets: dev, verify, test, bench, migrate, seed, smoke, fixtures:gen; verify chains fmt+lint+test+smoke; each target documented in comments.
  - **AC:**
    - [x] just verify runs all gates
    - [x] documented

- [x] **P0-006** — Commit conventions + CI check
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-005 · **Prio:** P1
  - **Spec:** Conventional-commit lint job; ticket id required in commit body (NFR-52); scope vocabulary documented (auth, engine, api, worker, web, ops).
  - **AC:**
    - [x] CI job active
    - [x] vocabulary documented

- [ ] **P0-007** — cargo-deny setup
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-006 · **Prio:** P1
  - **Spec:** deny.toml: advisories deny unpatched; licenses allow Apache/MIT/BSD/ISC/Unicode/MPL-2.0/Zlib, deny server-side AGPL; `cargo deny check` passes and joins CI.
  - **AC:**
    - [ ] cargo deny check green
    - [ ] CI wired

- [ ] **P0-008** — gitleaks secret scanning
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-007 · **Prio:** P1
  - **Spec:** gitleaks config + CI job + pre-commit hook; secret policy documented (PF_ env only, .env.example with dev defaults).
  - **AC:**
    - [ ] scan green
    - [ ] policy documented

- [ ] **P0-009** — Issue/PR templates
  - **Type:** docs · **Area:** repo · **Est:** S · **Deps:** P0-008 · **Prio:** P1
  - **Spec:** GitHub templates: bug (severity + repro fields), spike (research output expected), ticket-linking guidance to 07-PHASES; PR template auto-checklist.
  - **AC:**
    - [ ] templates committed

- [ ] **P0-010** — Release tooling (cocogitto)
  - **Type:** chore · **Area:** repo · **Est:** M · **Deps:** P0-009 · **Prio:** P1
  - **Spec:** cog config for conventional changelog + version bump; dry-run release generates CHANGELOG stub; release flow documented in deploy runbook.
  - **AC:**
    - [ ] cog dry-run works
    - [ ] flow documented

- [ ] **P0-011** — .env.example v1
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-010 · **Prio:** P1
  - **Spec:** All PF_* vars from runbook table with safe dev values; `just env:check` compares example vs code-referenced vars and fails on drift.
  - **AC:**
    - [ ] env:check green

- [ ] **P0-012** — Dev bootstrap (just dev)
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-011 · **Prio:** P1
  - **Spec:** One command starts compose deps, waits for readyz components, runs migrations, seeds, prints URLs.
  - **AC:**
    - [ ] fresh clone: just dev reaches healthy stack

- [ ] **P0-013** — CI smoke pipeline
  - **Type:** test · **Area:** repo · **Est:** M · **Deps:** P0-012 · **Prio:** P1
  - **Spec:** GitHub Actions: fmt, clippy, cargo deny, test (postgres+redis services), gitleaks; junit + coverage artifacts; README badge.
  - **AC:**
    - [ ] CI green on main
    - [ ] artifacts uploaded

- [ ] **P0-014** — Toolchain docs
  - **Type:** docs · **Area:** repo · **Est:** S · **Deps:** P0-013 · **Prio:** P1
  - **Spec:** Document pinned toolchain, MSRV, cargo-deny policy, release flow, version-update procedure (ADR on majors) in CONTRIBUTING.
  - **AC:**
    - [ ] docs committed

---

## E0.2 — Config & Errors (area: core/config)

- [ ] **P0-015** — Settings loader
  - **Type:** feat · **Area:** core/config · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** crates/core/src/config.rs: figment layered config (defaults < PF_* env < optional file), typed Settings with serde, validation (URLs parse, ranges hold).
  - **AC:**
    - [ ] invalid var fails boot with var name
    - [ ] precedence test green

- [ ] **P0-016** — Settings tests
  - **Type:** test · **Area:** core/config · **Est:** S · **Deps:** P0-015 · **Prio:** P1
  - **Spec:** Layer precedence, invalid var aggregated report, defaults snapshot; .env.example drift covered by `just env:check`.
  - **AC:**
    - [ ] tests green incl. aggregated-error case

- [ ] **P0-017** — Error taxonomy
  - **Type:** feat · **Area:** core/config · **Est:** M · **Deps:** P0-016 · **Prio:** P1
  - **Spec:** AppError enum (thiserror) with PF-codes per api-spec ranges; problem+json mapper; prod profile never leaks internals (trace-only).
  - **AC:**
    - [ ] each variant maps to status+code+title
    - [ ] prod body shape tested

- [ ] **P0-018** — Error mapping tests
  - **Type:** test · **Area:** core/config · **Est:** S · **Deps:** P0-017 · **Prio:** P1
  - **Spec:** Variant-to-response table test; debug vs prod body shape; request_id propagation into bodies.
  - **AC:**
    - [ ] table test green

- [ ] **P0-019** — Request-id middleware
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-018 · **Prio:** P1
  - **Spec:** UUIDv7 request ids, x-request-id echo, tracing span with request_id + route; log sampling configured.
  - **AC:**
    - [ ] integration test asserts id echo + span

- [ ] **P0-020** — Typed settings fail-fast report
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-019 · **Prio:** P1
  - **Spec:** Boot validation aggregates ALL invalid vars into one report (not first-failure); messages include runbook var names.
  - **AC:**
    - [ ] two invalid vars produce one report listing both

- [ ] **P0-021** — Static feature flags
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-020 · **Prio:** P1
  - **Spec:** PF_FEATURE_* env flags into typed struct; unknown flag warns; used by gpu + razorpay + provenance_strip (default off).
  - **AC:**
    - [ ] flag defaults off
    - [ ] unknown flag warns

- [ ] **P0-022** — DB-backed flags + cache
  - **Type:** feat · **Area:** core/config · **Est:** M · **Deps:** P0-021 · **Prio:** P1
  - **Spec:** feature_flags table + repo + Redis cache (30s TTL) + invalidation on admin edit; resolver trait hides the source.
  - **AC:**
    - [ ] flag flip visible within TTL
    - [ ] invalidation test green

- [ ] **P0-023** — Graceful shutdown
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-022 · **Prio:** P1
  - **Spec:** SIGTERM/SIGINT: stop accepting, drain in-flight with timeout, close pools; integration test sends signal to spawned server.
  - **AC:**
    - [ ] no dropped in-flight request
    - [ ] exit 0

- [ ] **P0-024** — Config reference doc
  - **Type:** docs · **Area:** core/config · **Est:** S · **Deps:** P0-023 · **Prio:** P1
  - **Spec:** Test/xtask generates settings table (name, default, used-by) and fails when .env.example or runbook drift.
  - **AC:**
    - [ ] generated table committed
    - [ ] drift test green

---

## E0.3 — Observability (area: observability)

- [ ] **P0-025** — Tracing bootstrap
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** JSON logs to stdout (pretty in dev), env-filtered levels, span fields (request_id, job_id, user_id) with PII redaction helper (never email / raw ip / bytes).
  - **AC:**
    - [ ] redaction helper unit tested
    - [ ] json logs parse

- [ ] **P0-026** — Prometheus /metrics
  - **Type:** feat · **Area:** observability · **Est:** S · **Deps:** P0-025 · **Prio:** P1
  - **Spec:** Internal-port route; process metrics + custom registry; network isolation in compose (internal only).
  - **AC:**
    - [ ] metrics scrape works
    - [ ] not on edge network

- [ ] **P0-027** — OpenTelemetry export
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** P0-026 · **Prio:** P1
  - **Spec:** tracing-opentelemetry OTLP exporter, no-op when PF_OTEL_ENDPOINT empty; exemplars wired for job stages.
  - **AC:**
    - [ ] exporter starts with endpoint set
    - [ ] no-op without

- [ ] **P0-028** — HTTP metrics middleware
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** P0-027 · **Prio:** P1
  - **Spec:** Requests total, duration histogram by route-template+method+status, inflight gauge; route templates only (no raw ids in labels).
  - **AC:**
    - [ ] histograms present with template labels

- [ ] **P0-029** — Job/queue metrics hooks
  - **Type:** feat · **Area:** observability · **Est:** S · **Deps:** P0-028 · **Prio:** P1
  - **Spec:** Queue depth gauge, enqueue/dequeue counters, stage timers exported from worker skeleton (values wired in later phases).
  - **AC:**
    - [ ] gauges/counters registered

- [ ] **P0-030** — Grafana dashboards as code
  - **Type:** chore · **Area:** observability · **Est:** M · **Deps:** P0-029 · **Prio:** P1
  - **Spec:** ops/grafana/dashboards: api-golden, queue, jobs; provisioning config; loaded in compose.
  - **AC:**
    - [ ] dashboards visible in grafana
    - [ ] provisioning reload documented

- [ ] **P0-031** — Prometheus config + alerts file
  - **Type:** chore · **Area:** observability · **Est:** S · **Deps:** P0-030 · **Prio:** P1
  - **Spec:** prometheus.yml scrape jobs; alert rules per runbook (readyz fail, queue age, error rate, disk); alertmanager optional, disabled by default.
  - **AC:**
    - [ ] rules load
    - [ ] targets up

- [ ] **P0-032** — Logging hygiene tests
  - **Type:** test · **Area:** observability · **Est:** S · **Deps:** P0-031 · **Prio:** P1
  - **Spec:** Integration test triggers auth-failure + job-error paths and asserts JSON logs contain no email/password/raw IP.
  - **AC:**
    - [ ] hygiene assertions green

- [ ] **P0-033** — Observability runbook section
  - **Type:** docs · **Area:** observability · **Est:** S · **Deps:** P0-032 · **Prio:** P1
  - **Spec:** How to read dashboards, what alerts mean, trace-id to request correlation; added to deploy runbook.
  - **AC:**
    - [ ] runbook section committed

---

## E0.4 — Postgres & sqlx (area: db)

- [ ] **P0-034** — Postgres in compose
  - **Type:** chore · **Area:** db · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** pg:16-alpine service, healthcheck, named volume, dev credentials in .env.example; just dev waits healthy.
  - **AC:**
    - [ ] healthy under compose
    - [ ] volume persists

- [ ] **P0-035** — sqlx wiring
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-034 · **Prio:** P1
  - **Spec:** apps/api + worker: pool builder (max conns, acquire timeout, statement cache), PF_DATABASE_URL; /readyz db component.
  - **AC:**
    - [ ] readyz reflects db health

- [ ] **P0-036** — Migration tooling
  - **Type:** chore · **Area:** db · **Est:** M · **Deps:** P0-035 · **Prio:** P1
  - **Spec:** sqlx migrate dir; just migrate (up), migrate:new, migrate:revert (dev only); CI runs clean migrate on empty db.
  - **AC:**
    - [ ] clean migrate green in CI

- [ ] **P0-037** — Migration 001 users+sessions
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-036 · **Prio:** P1
  - **Spec:** users, sessions per data-model doc (types, constraints, unique citext email); sqlx prepare outputs checked in.
  - **AC:**
    - [ ] migration applies + reverts (dev)
    - [ ] prepare fresh

- [ ] **P0-038** — Migration 002 jobs+artifacts
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-037 · **Prio:** P1
  - **Spec:** jobs, job_events, artifacts, quality_reports, uploads per data-model; indexes from doc.
  - **AC:**
    - [ ] migration applies
    - [ ] indexes present

- [ ] **P0-039** — Migration 003 billing
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-038 · **Prio:** P1
  - **Spec:** plans, subscriptions, usage_counters; plans seed with Free/Pro/Business entitlements jsonb matching FR-P-01.
  - **AC:**
    - [ ] seed rows correct

- [ ] **P0-040** — Migration 004 audit+flags+misc
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-039 · **Prio:** P1
  - **Spec:** audit_logs (append-only grants), feature_flags, idempotency_keys, email_outbox, attestations, abuse_reports, webhook_endpoints/deliveries.
  - **AC:**
    - [ ] migration applies
    - [ ] audit grants restrict UPDATE/DELETE

- [ ] **P0-041** — Repo layer tests
  - **Type:** test · **Area:** db · **Est:** M · **Deps:** P0-040 · **Prio:** P1
  - **Spec:** Testcontainers pg: repo roundtrips, constraint violations to typed errors, JSONB roundtrip; pool limits respected under parallelism.
  - **AC:**
    - [ ] suite green
    - [ ] no flaky pool errors

- [ ] **P0-042** — Tx helper
  - **Type:** feat · **Area:** db · **Est:** S · **Deps:** P0-041 · **Prio:** P1
  - **Spec:** with_tx helper (begin, closure, commit/rollback) with nesting guard; services use it; timeout configured.
  - **AC:**
    - [ ] rollback-on-error test green

- [ ] **P0-043** — Seed + reset tasks
  - **Type:** chore · **Area:** db · **Est:** S · **Deps:** P0-042 · **Prio:** P1
  - **Spec:** just seed (plans, flags, admin invite) idempotent; just db:reset for dev.
  - **AC:**
    - [ ] seed idempotent (run twice ok)

- [ ] **P0-044** — Slow-query log + index checklist
  - **Type:** chore · **Area:** db · **Est:** S · **Deps:** P0-043 · **Prio:** P1
  - **Spec:** pg slow-query log in dev; EXPLAIN checklist from data-model doc as a doc fixture; revisit at NFR-12.
  - **AC:**
    - [ ] log enabled
    - [ ] checklist committed

- [ ] **P0-045** — Schema docs automation
  - **Type:** docs · **Area:** db · **Est:** S · **Deps:** P0-044 · **Prio:** P1
  - **Spec:** xtask/test diffs migrations against 03-ARCHITECTURE/04-data-model.md table list; fails when a table exists in db but not doc.
  - **AC:**
    - [ ] drift test green

---

## E0.5 — Redis (area: redis)

- [ ] **P0-046** — Redis in compose
  - **Type:** chore · **Area:** redis · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** redis:7-alpine, healthcheck, appendonly off (cache role), maxmemory policy documented; /readyz component.
  - **AC:**
    - [ ] healthy under compose

- [ ] **P0-047** — Connection manager
  - **Type:** feat · **Area:** redis · **Est:** S · **Deps:** P0-046 · **Prio:** P1
  - **Spec:** deadpool-redis pool; key helper module with naming conventions (pf:{ns}:{key}) + TTL constants per retention table.
  - **AC:**
    - [ ] key helper unit tested

- [ ] **P0-048** — Rate-limit primitives
  - **Type:** feat · **Area:** redis · **Est:** M · **Deps:** P0-047 · **Prio:** P1
  - **Spec:** Fixed-window + token-bucket Lua scripts (atomic) per-ns limits; unit + integration tests race-safe under 50 concurrent.
  - **AC:**
    - [ ] no over-admission under race
    - [ ] scripts tested

- [ ] **P0-049** — Cache helper
  - **Type:** feat · **Area:** redis · **Est:** M · **Deps:** P0-048 · **Prio:** P1
  - **Spec:** get_or_set with TTL + singleflight (local mutex) to prevent stampede; invalidation helper; hit/miss metrics.
  - **AC:**
    - [ ] stampede test green
    - [ ] metrics emitted

- [ ] **P0-050** — SSE pubsub channel design
  - **Type:** feat · **Area:** redis · **Est:** S · **Deps:** P0-049 · **Prio:** P1
  - **Spec:** Redis pubsub channel per job id; publisher helper + subscriber stream; bounded channel, drop+log on slow consumer.
  - **AC:**
    - [ ] latency p95 <= 500ms in test
    - [ ] slow consumer drops safely

- [ ] **P0-051** — Redis integration tests
  - **Type:** test · **Area:** redis · **Est:** S · **Deps:** P0-050 · **Prio:** P1
  - **Spec:** testcontainers redis: primitives under concurrency, pubsub latency, failover error mapping to typed errors.
  - **AC:**
    - [ ] suite green

- [ ] **P0-052** — Redis ops notes
  - **Type:** docs · **Area:** redis · **Est:** S · **Deps:** P0-051 · **Prio:** P1
  - **Spec:** Key naming table, TTL conventions, memory expectations, degraded-mode behavior per NFR-13; in runbook.
  - **AC:**
    - [ ] runbook section committed

---

## E0.6 — Storage (area: storage)

- [ ] **P0-053** — StorageProvider trait
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Trait: presign_put_part/get, multipart init/complete/abort, head, delete, list_by_prefix; S3 impl via aws-sdk-s3; MinIO target for dev.
  - **AC:**
    - [ ] trait + impl compile
    - [ ] unit-tested with fixtures

- [ ] **P0-054** — MinIO in compose
  - **Type:** chore · **Area:** storage · **Est:** S · **Deps:** P0-053 · **Prio:** P1
  - **Spec:** minio service + mc bootstrap bucket creation, private policy; console port dev-only.
  - **AC:**
    - [ ] bucket private by default

- [ ] **P0-055** — Presigned flows
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P0-054 · **Prio:** P1
  - **Spec:** PUT part URLs (exp configurable), GET result URLs (per-plan TTL from FR-J-09), single-use where supported; expiry < token TTL guards.
  - **AC:**
    - [ ] expiry behavior tested

- [ ] **P0-056** — Key scheme + PII-free keys
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P0-055 · **Prio:** P1
  - **Spec:** ADR-0011 layout {env}/{yyyy-mm}/{job_id}/{artifact}.{ext}; helper unit-tested; traversal rejected.
  - **AC:**
    - [ ] no PII in keys
    - [ ] traversal rejected

- [ ] **P0-057** — Multipart session mgmt
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** P0-056 · **Prio:** P1
  - **Spec:** uploads table transitions pending_parts to ready to aborted; complete verifies part count + sizes.
  - **AC:**
    - [ ] state machine test green

- [ ] **P0-058** — Integrity checksums
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P0-057 · **Prio:** P1
  - **Spec:** Client-reported part sha256; server recomputes on complete (streaming); mismatch PF-2004; final object hash stored (FR-U-05).
  - **AC:**
    - [ ] mismatch rejected
    - [ ] hash stored

- [ ] **P0-059** — Content-type sniffing
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** P0-058 · **Prio:** P1
  - **Spec:** Magic-byte sniff on completed object (head+tail); allowlist png/jpeg/webp/mp4/mkv/webm; mismatch rejected.
  - **AC:**
    - [ ] polyglot test rejected

- [ ] **P0-060** — Lifecycle TTL policy
  - **Type:** chore · **Area:** storage · **Est:** M · **Deps:** P0-059 · **Prio:** P1
  - **Spec:** MinIO ILM script + docs for S3/R2 lifecycle for outputs per plan TTL + orphan GC fallback job.
  - **AC:**
    - [ ] ILM rule applied
    - [ ] GC job skeleton runs

- [ ] **P0-061** — Storage integration tests
  - **Type:** test · **Area:** storage · **Est:** M · **Deps:** P0-060 · **Prio:** P1
  - **Spec:** testcontainers minio: multipart happy path, part mismatch, presign expiry, delete, key traversal; offline mocks for CI.
  - **AC:**
    - [ ] suite green offline

- [ ] **P0-062** — Storage ops section
  - **Type:** docs · **Area:** storage · **Est:** S · **Deps:** P0-061 · **Prio:** P1
  - **Spec:** Buckets, keys, lifecycle, cost note, local console, pointing at R2/S3 (endpoint override).
  - **AC:**
    - [ ] runbook section committed

---

## E0.7 — API skeleton (area: api)

- [ ] **P0-063** — Axum server bootstrap
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** apps/api main: router, state (pool, redis, storage, settings, clock), tower layer order documented, two listeners (public 8080, metrics 8081).
  - **AC:**
    - [ ] server boots
    - [ ] layer order documented

- [ ] **P0-064** — Router module pattern
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-063 · **Prio:** P1
  - **Spec:** routes/{auth,uploads,jobs,billing,admin,meta}.rs own their nests; shared extractor set; compile-time route-table test (no duplicate paths).
  - **AC:**
    - [ ] route-table test green

- [ ] **P0-065** — Middleware chain
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** P0-064 · **Prio:** P1
  - **Spec:** Trace, request-id, body limit (uploads exempt via streaming), CORS (web origin env), secure headers, error mapper; order asserted by integration test.
  - **AC:**
    - [ ] order test green

- [ ] **P0-066** — Health endpoints
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-065 · **Prio:** P1
  - **Spec:** /healthz (process), /readyz (db, redis, storage, models with per-check latency); compose healthchecks consume them.
  - **AC:**
    - [ ] components report latency

- [ ] **P0-067** — OpenAPI scaffold
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** P0-066 · **Prio:** P1
  - **Spec:** utoipa derive setup, swagger-ui at /docs (dev+stage only), error schema components; CI diffs generated spec vs committed spec.
  - **AC:**
    - [ ] drift gate active

- [ ] **P0-068** — Pagination helpers
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-067 · **Prio:** P1
  - **Spec:** Cursor codec (base64 {ts,id}), limit clamp 1-100, Link header; tampered cursor returns 400.
  - **AC:**
    - [ ] unit tests incl. tamper

- [ ] **P0-069** — Body & size gates
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-068 · **Prio:** P1
  - **Spec:** Global JSON body limit 1MB; uploads route exempt (presign path only); oversize PF-2003.
  - **AC:**
    - [ ] oversize rejected

- [ ] **P0-070** — Auth extractor skeleton
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** P0-069 · **Prio:** P1
  - **Spec:** AuthContext extractor: bearer jwt OR api-key OR session cookie (web), role field; unimplemented paths return 501 until Phase 1 fills them.
  - **AC:**
    - [ ] extractor compiles + 501 paths documented

- [ ] **P0-071** — Integration harness
  - **Type:** test · **Area:** api · **Est:** M · **Deps:** P0-070 · **Prio:** P1
  - **Spec:** axum test client + testcontainers (pg, redis, minio) fixture builder: spawn_app, seed user, issue token, json() helper.
  - **AC:**
    - [ ] used by first endpoint tests

- [ ] **P0-072** — Request logging sampling
  - **Type:** chore · **Area:** api · **Est:** S · **Deps:** P0-071 · **Prio:** P1
  - **Spec:** Structured access log with sampling (success 1%, errors 100%) + slow-request log (>250ms) per NFR-1.
  - **AC:**
    - [ ] sampling verified

- [ ] **P0-073** — Error contract test
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-072 · **Prio:** P1
  - **Spec:** Golden test asserts problem+json shape, PF-code ranges, request_id presence for 401, 415, 429, 500.
  - **AC:**
    - [ ] golden shapes locked

- [ ] **P0-074** — API skeleton docs
  - **Type:** docs · **Area:** api · **Est:** S · **Deps:** P0-073 · **Prio:** P1
  - **Spec:** Route table generated into docs; how-to-add-endpoint checklist from repo-layout ownership map.
  - **AC:**
    - [ ] checklist committed

---

## E0.8 — Worker skeleton (area: worker)

- [ ] **P0-075** — apalis wiring
  - **Type:** feat · **Area:** worker · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** apps/worker: Redis-backed apalis workers (pipeline, cleanup, email, reconciliation queues), concurrency via env, versioned job envelope serde.
  - **AC:**
    - [ ] workers start
    - [ ] envelope version field present

- [ ] **P0-076** — Job envelope + registry
  - **Type:** feat · **Area:** worker · **Est:** S · **Deps:** P0-075 · **Prio:** P1
  - **Spec:** Typed envelope {job_id, kind, attempt, enqueued_at, params_hash}; registry maps kind to handler; unknown kind to DLQ + alert.
  - **AC:**
    - [ ] unknown-kind test quarantines safely

- [ ] **P0-077** — Retry policy
  - **Type:** feat · **Area:** worker · **Est:** M · **Deps:** P0-076 · **Prio:** P1
  - **Spec:** Per-queue retry counts, jittered exponential backoff (1/5/60min), max attempts to dead; retry events in job_events.
  - **AC:**
    - [ ] backoff schedule tested

- [ ] **P0-078** — Dead-letter handling
  - **Type:** feat · **Area:** worker · **Est:** S · **Deps:** P0-077 · **Prio:** P1
  - **Spec:** DLQ queue + admin listing endpoint stub (Phase 4 wires UI); poison message quarantined with payload snapshot.
  - **AC:**
    - [ ] poison quarantined
    - [ ] listing stub live

- [ ] **P0-079** — Visibility timeout + requeue
  - **Type:** feat · **Area:** worker · **Est:** S · **Deps:** P0-078 · **Prio:** P1
  - **Spec:** Apalis/redis visibility per NFR-21; requeue-stuck sweep (`just queue:requeue-stuck`); kill-mid-job drill lands Phase 3.
  - **AC:**
    - [ ] sweep runs
    - [ ] config documented

- [ ] **P0-080** — Heartbeat + worker metrics
  - **Type:** feat · **Area:** worker · **Est:** S · **Deps:** P0-079 · **Prio:** P1
  - **Spec:** Worker heartbeat rows (pid, queues, in-flight, started_at) + /metrics gauges; dashboard panel wired.
  - **AC:**
    - [ ] gauges visible

- [ ] **P0-081** — Graceful drain
  - **Type:** feat · **Area:** worker · **Est:** S · **Deps:** P0-080 · **Prio:** P1
  - **Spec:** SIGTERM: stop accepting, finish or checkpoint in-flight within timeout, exit 0; integration test asserts no lost job.
  - **AC:**
    - [ ] drain test green

- [ ] **P0-082** — Worker smoke job
  - **Type:** test · **Area:** worker · **Est:** S · **Deps:** P0-081 · **Prio:** P1
  - **Spec:** ping kind: enqueued by just smoke, processed, asserted done - proves api to redis to worker loop.
  - **AC:**
    - [ ] smoke green

---

## E0.9 — Docker & Compose (area: ops)

- [ ] **P0-083** — Dockerfile.api
  - **Type:** chore · **Area:** ops · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Multi-stage rust build (cargo-chef), slim runtime, non-root user, healthcheck; image < 150MB (NFR-63) enforced by CI check.
  - **AC:**
    - [ ] size gate green

- [ ] **P0-084** — Dockerfile.worker
  - **Type:** chore · **Area:** ops · **Est:** M · **Deps:** P0-083 · **Prio:** P1
  - **Spec:** Same base + models volume + ffmpeg runtime deps; startup downloads/verifies models (sha256 registry); size checked.
  - **AC:**
    - [ ] models verified at boot

- [ ] **P0-085** — compose.base.yml
  - **Type:** chore · **Area:** ops · **Est:** S · **Deps:** P0-084 · **Prio:** P1
  - **Spec:** Common env, networks (edge/internal), volumes; api, worker, web placeholder, postgres, redis, minio; profiles dev/prod.
  - **AC:**
    - [ ] profiles work

- [ ] **P0-086** — compose.dev.yml
  - **Type:** chore · **Area:** ops · **Est:** S · **Deps:** P0-085 · **Prio:** P1
  - **Spec:** Hot path: minio console, mailhog, swagger, bacon/cargo-watch documented (host side), relaxed limits, seeded data.
  - **AC:**
    - [ ] dev stack conveniences live

- [ ] **P0-087** — compose.prod.yml
  - **Type:** chore · **Area:** ops · **Est:** S · **Deps:** P0-086 · **Prio:** P1
  - **Spec:** caddy edge (auto-TLS), internal isolation, resource limits (mem caps), restart policies, log rotation.
  - **AC:**
    - [ ] prod compose validates

- [ ] **P0-088** — Healthchecks everywhere
  - **Type:** chore · **Area:** ops · **Est:** S · **Deps:** P0-087 · **Prio:** P1
  - **Spec:** Every service has a healthcheck; compose waits; just smoke fails if any service unhealthy after 120s.
  - **AC:**
    - [ ] smoke enforces health

- [ ] **P0-089** — .env.example + compose wiring
  - **Type:** chore · **Area:** ops · **Est:** S · **Deps:** P0-088 · **Prio:** P1
  - **Spec:** All PF_* + third-party vars referenced by compose; no literal secrets in yml (CI grep).
  - **AC:**
    - [ ] grep gate green

- [ ] **P0-090** — Smoke script
  - **Type:** test · **Area:** ops · **Est:** S · **Deps:** P0-089 · **Prio:** P1
  - **Spec:** `just smoke`: compose up -d, wait healthz/readyz all services, enqueue ping job, assert processed, print URLs; runs in CI.
  - **AC:**
    - [ ] smoke green in CI

- [ ] **P0-091** — Ops quickstart
  - **Type:** docs · **Area:** ops · **Est:** S · **Deps:** P0-090 · **Prio:** P1
  - **Spec:** README ops section: 3 commands to working stack, ports table, troubleshooting matrix (top 5 failures).
  - **AC:**
    - [ ] quickstart committed

---

## E0.10 — Test harness & fixtures (area: testing)

- [ ] **P0-092** — testcontainers fixtures
  - **Type:** feat · **Area:** testing · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Shared test ctx: pg (migrated), redis, minio bucketed; spawn_app helper; per-test isolation (schema-per-test or cleanup); parallel-safe.
  - **AC:**
    - [ ] parallel suite stable

- [ ] **P0-093** — Golden media generator
  - **Type:** chore · **Area:** testing · **Est:** M · **Deps:** P0-092 · **Prio:** P1
  - **Spec:** `just fixtures:gen` synthesizes images/videos deterministically (seeded): 24 images matrix + 3 videos + expected-metrics envelopes; checked in; gen reproducible (sha table).
  - **AC:**
    - [ ] gen twice = same shas
    - [ ] fixture set committed

- [ ] **P0-094** — Template pack generator
  - **Type:** chore · **Area:** testing · **Est:** S · **Deps:** P0-093 · **Prio:** P1
  - **Spec:** Synthesized badge/text templates with alpha + manifest (name, sha256, params); dev pack labeled SYNTHETIC-TEST-ONLY; production pack from same tooling (no scraping).
  - **AC:**
    - [ ] manifest checksums verified

- [ ] **P0-095** — Snapshot testing (insta)
  - **Type:** feat · **Area:** testing · **Est:** S · **Deps:** P0-094 · **Prio:** P1
  - **Spec:** cargo-insta for JSON responses (problem+json, reports); review flow documented.
  - **AC:**
    - [ ] snapshots accepted

- [ ] **P0-096** — proptest harness
  - **Type:** feat · **Area:** testing · **Est:** S · **Deps:** P0-095 · **Prio:** P1
  - **Spec:** Property tests scaffold: mask codec roundtrip, cursor codec, key scheme; shrink-friendly; nightly job.
  - **AC:**
    - [ ] properties hold 1000 cases

- [ ] **P0-097** — Criterion benches scaffold
  - **Type:** chore · **Area:** testing · **Est:** M · **Deps:** P0-096 · **Prio:** P1
  - **Spec:** crates/engine/benches: detect, inpaint tiles, encode, full-job per resolution tier; baselines stored; just bench documented.
  - **AC:**
    - [ ] bench runs
    - [ ] baselines committed

- [ ] **P0-098** — llvm-cov + coverage gate
  - **Type:** chore · **Area:** testing · **Est:** M · **Deps:** P0-097 · **Prio:** P1
  - **Spec:** Coverage job per NFR-50 floors; report-only until Phase 2, then fails below floors.
  - **AC:**
    - [ ] report live
    - [ ] gate flag documented

- [ ] **P0-099** — Flaky policy tooling
  - **Type:** test · **Area:** testing · **Est:** S · **Deps:** P0-098 · **Prio:** P1
  - **Spec:** just test:repeat <name> <n> helper; quarantine tag convention documented; nightly job lists quarantined tests.
  - **AC:**
    - [ ] helper works

---

## E0.11 — Security baseline (area: security)

- [ ] **P0-100** — cargo-audit/deny in CI
  - **Type:** chore · **Area:** security · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Advisory gate + weekly scheduled run; unpatched critical = red build; policy in deny.toml comments.
  - **AC:**
    - [ ] gate green

- [ ] **P0-101** — Input validation layer
  - **Type:** feat · **Area:** security · **Est:** M · **Deps:** P0-100 · **Prio:** P1
  - **Spec:** validator crate + custom rules (filename sanitize, email, public-only url); rejects 422 problem+json with field errors.
  - **AC:**
    - [ ] field errors shaped

- [ ] **P0-102** — Secure headers middleware
  - **Type:** feat · **Area:** security · **Est:** S · **Deps:** P0-101 · **Prio:** P1
  - **Spec:** HSTS (prod), X-Content-Type-Options, Referrer-Policy, CSP for api-served docs, frame-ancestors none; header table test.
  - **AC:**
    - [ ] header table test green

- [ ] **P0-103** — Rate limiter skeleton
  - **Type:** feat · **Area:** security · **Est:** M · **Deps:** P0-102 · **Prio:** P1
  - **Spec:** Redis token-bucket service keyed {scope,id}; scopes ip/user/api-key/route; 429 + Retry-After; middleware + race-safe tests.
  - **AC:**
    - [ ] no over-admission under race

- [ ] **P0-104** — Body/size/dimension guards
  - **Type:** feat · **Area:** security · **Est:** S · **Deps:** P0-103 · **Prio:** P1
  - **Spec:** Content-Length caps per route class; dimension caps at job-create (FR-U-04) with PF-2003/PF-2005.
  - **AC:**
    - [ ] guards tested

- [ ] **P0-105** — Secrets policy enforcement
  - **Type:** chore · **Area:** security · **Est:** S · **Deps:** P0-104 · **Prio:** P1
  - **Spec:** CI: no PF_ secrets in code (gitleaks), compose has no literals; rotation runbook section drafted (JWT dual-verify, S3 keys).
  - **AC:**
    - [ ] gates green
    - [ ] runbook drafted

- [ ] **P0-106** — Threat model stub
  - **Type:** chore · **Area:** security · **Est:** S · **Deps:** P0-105 · **Prio:** P1
  - **Spec:** docs/threat-model.md: assets, actors, entry points (upload, webhooks, admin), trust boundaries; STRIDE-lite table; living doc.
  - **AC:**
    - [ ] stub committed

- [ ] **P0-107** — Security smoke tests
  - **Type:** test · **Area:** security · **Est:** S · **Deps:** P0-106 · **Prio:** P1
  - **Spec:** Headers present, rate limit trips at threshold, oversized body rejected, method-not-allowed, no swagger in prod profile.
  - **AC:**
    - [ ] smoke suite green

- [ ] **P0-108** — Security baseline summary
  - **Type:** docs · **Area:** security · **Est:** S · **Deps:** P0-107 · **Prio:** P1
  - **Spec:** What is enforced where (CI + runtime + ops); checklist mapping to 05-QUALITY/03-security-testing.md items for Phase 1+.
  - **AC:**
    - [ ] summary committed
