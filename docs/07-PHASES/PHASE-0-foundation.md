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

- [x] **P0-007** — cargo-deny setup
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-006 · **Prio:** P1
  - **Spec:** deny.toml: advisories deny unpatched; licenses allow Apache/MIT/BSD/ISC/Unicode/MPL-2.0/Zlib, deny server-side AGPL; `cargo deny check` passes and joins CI.
  - **AC:**
    - [x] cargo deny check green
    - [x] CI wired

- [x] **P0-008** — gitleaks secret scanning
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-007 · **Prio:** P1
  - **Spec:** gitleaks config + CI job + pre-commit hook; secret policy documented (PF_ env only, .env.example with dev defaults).
  - **AC:**
    - [x] scan green
    - [x] policy documented

- [x] **P0-009** — Issue/PR templates
  - **Type:** docs · **Area:** repo · **Est:** S · **Deps:** P0-008 · **Prio:** P1
  - **Spec:** GitHub templates: bug (severity + repro fields), spike (research output expected), ticket-linking guidance to 07-PHASES; PR template auto-checklist.
  - **AC:**
    - [x] templates committed

- [x] **P0-010** — Release tooling (cocogitto)
  - **Type:** chore · **Area:** repo · **Est:** M · **Deps:** P0-009 · **Prio:** P1
  - **Spec:** cog config for conventional changelog + version bump; dry-run release generates CHANGELOG stub; release flow documented in deploy runbook.
  - **AC:**
    - [x] cog dry-run works
    - [x] flow documented

- [x] **P0-011** — .env.example v1
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-010 · **Prio:** P1
  - **Spec:** All PF_* vars from runbook table with safe dev values; `just env:check` compares example vs code-referenced vars and fails on drift.
  - **AC:**
    - [x] env:check green

- [x] **P0-012** — Dev bootstrap (just dev)
  - **Type:** chore · **Area:** repo · **Est:** S · **Deps:** P0-011 · **Prio:** P1
  - **Spec:** One command starts compose deps, waits for readyz components, runs migrations, seeds, prints URLs.
  - **AC:**
    - [x] fresh clone: just dev reaches healthy stack

- [x] **P0-013** — CI smoke pipeline
  - **Type:** test · **Area:** repo · **Est:** M · **Deps:** P0-012 · **Prio:** P1
  - **Spec:** GitHub Actions: fmt, clippy, cargo deny, test (postgres+redis services), gitleaks; junit + coverage artifacts; README badge.
  - **AC:**
    - [x] CI green on main
    - [x] artifacts uploaded

- [x] **P0-014** — Toolchain docs
  - **Type:** docs · **Area:** repo · **Est:** S · **Deps:** P0-013 · **Prio:** P1
  - **Spec:** Document pinned toolchain, MSRV, cargo-deny policy, release flow, version-update procedure (ADR on majors) in CONTRIBUTING.
  - **AC:**
    - [x] docs committed

---

## E0.2 — Config & Errors (area: core/config)

- [x] **P0-015** — Settings loader
  - **Type:** feat · **Area:** core/config · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** crates/core/src/config.rs: figment layered config (defaults < PF_* env < optional file), typed Settings with serde, validation (URLs parse, ranges hold).
  - **AC:**
    - [x] invalid var fails boot with var name
    - [x] precedence test green

- [x] **P0-016** — Settings tests
  - **Type:** test · **Area:** core/config · **Est:** S · **Deps:** P0-015 · **Prio:** P1
  - **Spec:** Layer precedence, invalid var aggregated report, defaults snapshot; .env.example drift covered by `just env:check`.
  - **AC:**
    - [x] tests green incl. aggregated-error case

- [x] **P0-017** — Error taxonomy
  - **Type:** feat · **Area:** core/config · **Est:** M · **Deps:** P0-016 · **Prio:** P1
  - **Spec:** AppError enum (thiserror) with PF-codes per api-spec ranges; problem+json mapper; prod profile never leaks internals (trace-only).
  - **AC:**
    - [x] each variant maps to status+code+title
    - [x] prod body shape tested

- [x] **P0-018** — Error mapping tests
  - **Type:** test · **Area:** core/config · **Est:** S · **Deps:** P0-017 · **Prio:** P1
  - **Spec:** Variant-to-response table test; debug vs prod body shape; request_id propagation into bodies.
  - **AC:**
    - [x] table test green

- [x] **P0-019** — Request-id middleware
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-018 · **Prio:** P1
  - **Spec:** UUIDv7 request ids, x-request-id echo, tracing span with request_id + route; log sampling configured.
  - **AC:**
    - [x] integration test asserts id echo + span

- [x] **P0-020** — Typed settings fail-fast report
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-019 · **Prio:** P1
  - **Spec:** Boot validation aggregates ALL invalid vars into one report (not first-failure); messages include runbook var names.
  - **AC:**
    - [x] two invalid vars produce one report listing both

- [x] **P0-021** — Static feature flags
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-020 · **Prio:** P1
  - **Spec:** PF_FEATURE_* env flags into typed struct; unknown flag warns; used by gpu + razorpay + provenance_strip (default off).
  - **AC:**
    - [x] flag defaults off
    - [x] unknown flag warns

- [x] **P0-022** — DB-backed flags + cache
  - **Type:** feat · **Area:** core/config · **Est:** M · **Deps:** P0-021 · **Prio:** P1
  - **Spec:** feature_flags table + repo + Redis cache (30s TTL) + invalidation on admin edit; resolver trait hides the source.
  - **AC:**
    - [x] flag flip visible within TTL
    - [x] invalidation test green

- [x] **P0-023** — Graceful shutdown
  - **Type:** feat · **Area:** core/config · **Est:** S · **Deps:** P0-022 · **Prio:** P1
  - **Spec:** SIGTERM/SIGINT: stop accepting, drain in-flight with timeout, close pools; integration test sends signal to spawned server.
  - **AC:**
    - [x] no dropped in-flight request
    - [x] exit 0

- [x] **P0-024** — Config reference doc
  - **Type:** docs · **Area:** core/config · **Est:** S · **Deps:** P0-023 · **Prio:** P1
  - **Spec:** Test/xtask generates settings table (name, default, used-by) and fails when .env.example or runbook drift.
  - **AC:**
    - [x] generated table committed
    - [x] drift test green

---

## E0.3 — Observability (area: observability)

- [x] **P0-025** — Tracing bootstrap
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** JSON logs to stdout (pretty in dev), env-filtered levels, span fields (request_id, job_id, user_id) with PII redaction helper (never email / raw ip / bytes).
  - **AC:**
    - [x] redaction helper unit tested
    - [x] json logs parse

- [x] **P0-026** — Prometheus /metrics
  - **Type:** feat · **Area:** observability · **Est:** S · **Deps:** P0-025 · **Prio:** P1
  - **Spec:** Internal-port route; process metrics + custom registry; network isolation in compose (internal only).
  - **AC:**
    - [x] metrics scrape works
    - [x] not on edge network

- [x] **P0-027** — OpenTelemetry export
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** P0-026 · **Prio:** P1
  - **Spec:** tracing-opentelemetry OTLP exporter, no-op when PF_OTEL_ENDPOINT empty; exemplars wired for job stages.
  - **AC:**
    - [x] exporter starts with endpoint set
    - [x] no-op without

- [x] **P0-028** — HTTP metrics middleware
  - **Type:** feat · **Area:** observability · **Est:** M · **Deps:** P0-027 · **Prio:** P1
  - **Spec:** Requests total, duration histogram by route-template+method+status, inflight gauge; route templates only (no raw ids in labels).
  - **AC:**
    - [x] histograms present with template labels

- [x] **P0-029** — Job/queue metrics hooks
  - **Type:** feat · **Area:** observability · **Est:** S · **Deps:** P0-028 · **Prio:** P1
  - **Spec:** Queue depth gauge, enqueue/dequeue counters, stage timers exported from worker skeleton (values wired in later phases).
  - **AC:**
    - [x] gauges/counters registered

- [x] **P0-030** — Grafana dashboards as code
  - **Type:** chore · **Area:** observability · **Est:** M · **Deps:** P0-029 · **Prio:** P1
  - **Spec:** ops/grafana/dashboards: api-golden, queue, jobs; provisioning config; loaded in compose.
  - **AC:**
    - [x] dashboards visible in grafana — statically validated only; live visibility deferred to E0.9 per ASSUMPTION-004 (no Grafana/compose on this VM yet)
    - [x] provisioning reload documented

- [x] **P0-031** — Prometheus config + alerts file
  - **Type:** chore · **Area:** observability · **Est:** S · **Deps:** P0-030 · **Prio:** P1
  - **Spec:** prometheus.yml scrape jobs; alert rules per runbook (readyz fail, queue age, error rate, disk); alertmanager optional, disabled by default.
  - **AC:**
    - [x] rules load — verified by promtool 3.5.0 (config valid, 1 rule file, 2 rules) + static validator in CI
    - [x] targets up — statically defined; live confirmation deferred to E0.9 per ASSUMPTION-005 (no Prometheus running yet)

- [x] **P0-032** — Logging hygiene tests
  - **Type:** test · **Area:** observability · **Est:** S · **Deps:** P0-031 · **Prio:** P1
  - **Spec:** Integration test triggers auth-failure + job-error paths and asserts JSON logs contain no email/password/raw IP.
  - **AC:**
    - [x] hygiene assertions green

- [x] **P0-033** — Observability runbook section
  - **Type:** docs · **Area:** observability · **Est:** S · **Deps:** P0-032 · **Prio:** P1
  - **Spec:** How to read dashboards, what alerts mean, trace-id to request correlation; added to deploy runbook.
  - **AC:**
    - [x] runbook section committed

---

## E0.4 — Postgres & sqlx (area: db)

- [ ] **P0-034** — Postgres in compose
  - **Type:** chore · **Area:** db · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** pg:16-alpine service, healthcheck, named volume, dev credentials in .env.example; just dev waits healthy.
  - **AC:**
    - [ ] healthy under compose
    - [ ] volume persists
  - **Status:** implementation committed (compose file, env vars, dev.sh wiring with compose-first/native fallback + pg_isready wait, `just compose-db-check` + CI step — all verified statically; `just dev` reaches "stack up" with native postgres). Live ACs blocked by DOCKER-001 (no container can start on this VM) — verification deferred to CI/E0.9 smoke. See docs/09-STATE/ISSUES.md.

- [x] **P0-035** — sqlx wiring
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-034 · **Prio:** P1
  - **Spec:** apps/api + worker: pool builder (max conns, acquire timeout, statement cache), PF_DATABASE_URL; /readyz db component.
  - **AC:**
    - [x] readyz reflects db health

- [x] **P0-036** — Migration tooling
  - **Type:** chore · **Area:** db · **Est:** M · **Deps:** P0-035 · **Prio:** P1
  - **Spec:** sqlx migrate dir; just migrate (up), migrate:new, migrate:revert (dev only); CI runs clean migrate on empty db.
  - **AC:**
    - [x] clean migrate green in CI
  - **Status:** `just migrate` verified locally against a fresh scratch database (applies clean, second run is a no-op) and the CI step is committed; a green CI run itself is unobserved until the parent pushes — the workflow-file push is still blocked (see the push gap in the run report).

- [x] **P0-037** — Migration 001 users+sessions
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-036 · **Prio:** P1
  - **Spec:** users, sessions per data-model doc (types, constraints, unique citext email); sqlx prepare outputs checked in.
  - **AC:**
    - [x] migration applies + reverts (dev)
    - [x] prepare fresh
  - **Status:** `migrations/20261112064220_users_sessions.{up,down}.sql` — users (citext email UNIQUE, argon2id password_hash, role/status CHECKs, tos_version, deleted_at, settings jsonb) + sessions (refresh_hash UNIQUE, family_id, device jsonb {ua, ip_hash}, expires_at, revoked_at, replaced_by self-FK). Verified on a fresh scratch DB: applies clean, case-insensitive email unique rejects `alice@example.com` after `Alice@Example.com`, role/status CHECKs + sessions FK reject bad rows, revert drops both tables, re-apply restores them; committed live test `crates/core/tests/migration_001_users_sessions.rs` covers all of it. Dev DB migrated. `cargo sqlx prepare --workspace` and `--check` both pass, and `just sqlx-prepare[-check]` + a CI step (after migrate) are committed — BUT there are no `query!` macros in the codebase yet, so prepare reports "no queries found" and generates no `.sqlx/` files to check in; the gate is in place and becomes real when the repo layer lands (P0-041). sqlx-cli 0.9.0 installed locally for this; CI installs it.

- [x] **P0-038** — Migration 002 jobs+artifacts
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-037 · **Prio:** P1
  - **Spec:** jobs, job_events, artifacts, quality_reports, uploads per data-model; indexes from doc.
  - **AC:**
    - [x] migration applies
    - [x] indexes present
  - **Status:** `migrations/20261112064924_jobs_artifacts.{up,down}.sql` — uploads (kind/status CHECKs), jobs (kind CHECK, progress 0..1, idempotency_key UNIQUE, upload_id nullable for batch jobs), job_events (type CHECK), artifacts (type CHECK, expires_at NOT NULL), quality_reports (UNIQUE job_id, gate pass|fail). Doc's 3 indexes + jobs(upload_id), job_events(job_id, at), artifacts(job_id). No CHECK on jobs.status/stage/mode/lane values: the docs name no complete state machine (only queued/running/succeeded/quality_failed in passing) — the machine is defined with the pipeline (Phase 3). jobs.attestation_id is a plain UUID for now; the FK lands in P0-040 with the attestations table. Committed live test `crates/core/tests/migration_002_jobs_artifacts.rs` (schema, indexes, CHECKs, idempotency unique, 1-1 report, user-delete CASCADE, revert/re-apply). Dev DB migrated (versions [1, 20261112064220, 20261112064924]). Also isolated both migration live tests to scratch dirs holding only the pairs they need: the P0-037 test broke when 002 landed (revert-last reverted 002, not 001).

- [x] **P0-039** — Migration 003 billing
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-038 · **Prio:** P1
  - **Spec:** plans, subscriptions, usage_counters; plans seed with Free/Pro/Business entitlements jsonb matching FR-P-01.
  - **AC:**
    - [x] seed rows correct
  - **Status:** `migrations/20261112065140_billing.{up,down}.sql` — plans (code UNIQUE, amount in paise: Free 0 / Pro 49900 / Business 199900, stripe_price_id NULL until live prices exist), subscriptions (provider stripe|razorpay, status CHECK, plan_id RESTRICT), usage_counters PK (user_id, day). Seed rows verified against FR-P-01 in the live test `crates/core/tests/migration_003_billing.rs` (all 7 entitlement fields per plan, features flags, seed re-run is a no-op, provider/status CHECKs, plan RESTRICT, counter PK + non-negative, revert/re-apply reseeds). Dev DB migrated; plans read back: free 0/10/standard, pro 49900/500/standard, business 199900/5000/priority.

- [x] **P0-040** — Migration 004 audit+flags+misc
  - **Type:** feat · **Area:** db · **Est:** M · **Deps:** P0-039 · **Prio:** P1
  - **Spec:** audit_logs (append-only grants), feature_flags, idempotency_keys, email_outbox, attestations, abuse_reports, webhook_endpoints/deliveries.
  - **AC:**
    - [x] migration applies
  - **Status:** `migrations/20261112065339_audit_flags_misc.{up,down}.sql` — audit_logs, idempotency_keys, email_outbox, attestations, abuse_reports, webhook_endpoints/deliveries, api_keys + the deferred jobs.attestation_id FK (ON DELETE SET NULL). feature_flags NOT recreated (migration 0001 already created it). api_keys has no ticket of its own in Phase 0/1, so it lands here under "misc" (flagged call — the data-model doc specifies it and Phase 1's API-key work needs it). Append-only is enforced by a trigger (forbid_append_only_mutation) on audit_logs, job_events (deferred from P0-038) and attestations (immutable per doc) — NOT by GRANTs: the dev DB has a single owner role (pf) that bypasses ACLs, so a REVOKE would be a no-op; true role separation is a production-deployment concern. Live test `crates/core/tests/migration_004_audit_flags_misc.rs` covers tables, doc indexes, all three triggers, the FK, status CHECK, idempotency PK, revert/re-apply. The test caught a real bug: the first down migration left dangling jobs.attestation_id values, so re-apply's FK validation failed — fixed by NULLing them in the down migration before dropping attestations. Dev DB migrated: 20 tables, versions [1, 20261112064220, 20261112064924, 20261112065140, 20261112065339].
    - [ ] audit grants restrict UPDATE/DELETE

- [x] **P0-041** — Repo layer tests
  - **Type:** test · **Area:** db · **Est:** M · **Deps:** P0-040 · **Prio:** P1
  - **Spec:** Testcontainers pg: repo roundtrips, constraint violations to typed errors, JSONB roundtrip; pool limits respected under parallelism.
  - **AC:**
    - [x] suite green
    - [x] no flaky pool errors
  - **Status:** `crates/core/src/repos/` — `users` (create/by_id/by_email), `jobs` (create/by_id/list_by_user/set_status), `plans` (by_code/all), all `query_as!` with compile-time checking; `RepoError` maps Postgres codes 23505/23514/23503 to Unique/Check/ForeignKeyViolation with the constraint name, plus NotFound. Tests in `crates/core/tests/repos_live.rs` (6 tests: user roundtrip + case-insensitive lookup, duplicate email typed error, job roundtrip + params JSONB + set_status/NotFound, job constraint violations typed, plans + entitlements JSONB, 10 concurrent lookups on a 2-connection pool all succeed). Testcontainers NOT used: docker cannot start containers on this VM (DOCKER-001), so the tests run against native postgres with per-test scratch DBs (same pattern as the migration tests); the Testcontainers form belongs with the fixtures work (P0-092). The tests caught a real bug: a bound `text` parameter makes `email = $1` resolve as case-sensitive `text = text`, so `by_email` uses `$1::citext` (verified: parameterized query returned 0 rows without the cast, 1 with it). `cargo sqlx prepare` now generates real `.sqlx/` data (9 query files, checked in); `SQLX_OFFLINE=true` is exported in the justfile and set in CI's verify job so builds no longer need DATABASE_URL; `just sqlx-prepare-check` passes. Workspace sqlx features gained `macros`/`chrono`/`uuid`/`json` for the query macros.

- [x] **P0-042** — Tx helper
  - **Type:** feat · **Area:** db · **Est:** S · **Deps:** P0-041 · **Prio:** P1
  - **Spec:** with_tx helper (begin, closure, commit/rollback) with nesting guard; services use it; timeout configured.
  - **AC:**
    - [x] rollback-on-error test green
  - **Status:** `crates/core/src/tx.rs` — `with_tx(pool, timeout, f)`: begin, `SET LOCAL statement_timeout` (from new `PoolConfig.transaction_timeout`, default 30 s, `Duration::ZERO` disables), commit on Ok, rollback on Err; `TxError::{Nested, Begin, Commit, Inner}`. Nesting guard is a tokio task-local depth counter. The closure receives `&mut PgConnection` (the tx's connection) and returns a boxed future (`|conn| Box::pin(async move { ... })`) — passing `&mut Transaction` does not compile (two independent lifetimes, `&mut` invariant over the transaction's own), and neither plain `FnOnce -> Fut` nor `AsyncFnOnce` can express the borrow through the closure's reborrow; the boxed future makes the borrow lifetime part of the type. Repos changed to `impl Executor` so the same call works against `&pool` and `&mut *conn` inside `with_tx`. Tests in `crates/core/tests/tx_live.rs` (4 tests: commit writes both rows, rollback writes nothing, nesting guard fails the inner call while the outer commits, 10 ms timeout cancels a 1 s `pg_sleep` with 57014). No service layer exists yet in Phase 0, so "services use it" lands with Phase 1 services.

- [x] **P0-043** — Seed + reset tasks
  - **Type:** chore · **Area:** db · **Est:** S · **Deps:** P0-042 · **Prio:** P1
  - **Spec:** just seed (plans, flags, admin invite) idempotent; just db:reset for dev.
  - **AC:**
    - [x] seed idempotent (run twice ok)
  - **Status:** `scripts/seed.sql` + `crates/core/src/seed.rs` (`seed(pool) -> SeedReport { plans, flags }`, `include_str!` of the SQL) + `crates/core/examples/seed.rs` (`just seed`: "seeded: 3 plans, 3 feature flags"). Plans are verbatim the migration 003 seed (FR-P-01, amounts in paise); flags are the 3 known keys (razorpay, provenance_strip, gpu), all OFF with updated_by 'seed'. Every INSERT is ON CONFLICT DO NOTHING — verified by live test `crates/core/tests/seed_live.rs` (run twice same counts; an admin plan edit + flag flip survive a re-seed). `just db::reset` (module `db.just`, `mod db` in the justfile) drops + recreates the local dev DB, migrates, seeds — implemented as a `reset` arm in `crates/core/examples/migrate.rs` (localhost-only guard, bare-identifier check on the DB name, `AssertSqlSafe` with an audit comment; DROP/CREATE run as two single `query` calls because `raw_sql` wraps in a transaction block and DROP DATABASE cannot run inside one). Verified end-to-end on the dev DB (recreated `pf`, 5 migrations, 3 plans + 3 flags OFF, second `just seed` a no-op) and the non-local refusal (exit 2). NOT done: the "admin invite" — no invites table or invite flow exists in the kit and the runbook's `pf-admin-create` CLI has no owning ticket; the seed invents no admin user/credential (DECISIONS.md #8, owner-input-needed). NOTE: the ticket spells the command `just db:reset` (single colon) but just 1.42 does not accept it — `just db::reset` (or `just db reset`) is the real invocation.

- [x] **P0-044** — Slow-query log + index checklist
  - **Type:** chore · **Area:** db · **Est:** S · **Deps:** P0-043 · **Prio:** P1
  - **Spec:** pg slow-query log in dev; EXPLAIN checklist from data-model doc as a doc fixture; revisit at NFR-12.
  - **AC:**
    - [x] log enabled
    - [x] checklist committed
  - **Status:** Threshold is the NFR-12 bar: `log_min_duration_statement = 50ms`. Compose: `command: ["postgres", "-c", "log_min_duration_statement=50ms"]` in `ops/compose.db.yml` (`just compose-db-check` green). Native dev DB: cluster-wide in `/etc/postgresql/16/main/postgresql.conf` + `pg_reload_conf()` — cluster-wide (not ALTER DATABASE) because the `pf` role is not a superuser (`ALTER DATABASE ... SET` is refused for this SUSET parameter) and a cluster setting survives `just db::reset`. Verified live: `SHOW` returns 50ms, `pg_sleep(0.06)` logged `duration: 60.710 ms` in `/var/log/postgresql/postgresql-16-main.log`, a fast query adds no line. Checklist: `docs/05-QUALITY/04-slow-query-checklist.md` — all 10 doc-listed indexes, each EXPLAIN (ANALYZE)-verified on a scratch DB (200 users / 20k jobs / ANALYZE): 8 index-backed directly, `api_keys` seq-scanned at 200 rows (planner cost — index proven with `enable_seqscan = off`, 0.019 ms), `audit_logs` actor + `webhook_deliveries` endpoint used Bitmap Index Scan + quicksort. Revisit at NFR-12 (100k jobs/day load) recorded in the doc.

- [x] **P0-045** — Schema docs automation
  - **Type:** docs · **Area:** db · **Est:** S · **Deps:** P0-044 · **Prio:** P1
  - **Spec:** xtask/test diffs migrations against 03-ARCHITECTURE/04-data-model.md table list; fails when a table exists in db but not doc.
  - **AC:**
    - [x] drift test green
  - **Status:** `scripts/check_schema_docs.py` (+ `just schema-docs-check`, CI step after config-table-check): parses `CREATE TABLE` from every `*.sql` except `*.down.sql` and the `**<name>**` lines under the doc's `## Tables` section; fails (exit 1) when a table is in migrations but not the doc, or vice versa; `_sqlx_migrations` excluded (sqlx-internal). Static only — no DB needed. Follows the P0-024 precedent (Python script + just recipe + CI step) instead of a Rust xtask — no xtask crate exists in the workspace. Unit tests in `scripts/test_check_schema_docs.py` (6 tests, incl. the real-repo drift test: 19 tables agree). Verified the gate fails: a drifted copy (extra `zebra_stripes` table + deleted `abuse_reports` doc line) exits 1 naming both.

---

## E0.5 — Redis (area: redis)

- [ ] **P0-046** — Redis in compose
  - **Type:** chore · **Area:** redis · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** redis:7-alpine, healthcheck, appendonly off (cache role), maxmemory policy documented; /readyz component.
  - **AC:**
    - [ ] healthy under compose
  - **Status:** implementation committed (compose file, /readyz redis component, dev.sh wiring, `just compose-redis-check` + CI step — all verified; see below). Live AC blocked by DOCKER-001: `docker compose -f ops/compose.redis.yml up -d --wait` fails with "failed to create default sandbox: operation not permitted" (verified 2026-11-12) — verification deferred to CI/E0.9 smoke, same convention as P0-034. Details: `ops/compose.redis.yml` (redis:7-alpine, host networking per ADR-0014, `redis-cli ping` healthcheck, `command: redis-server --appendonly no --save "" --maxmemory 256mb --maxmemory-policy allkeys-lru`, no volume — cache role; maxmemory policy documented in the file header). `crates/core/src/redis.rs`: `redis_health(url)` — raw RESP PING over TCP, 2 s timeout, 5 unit tests against a fake TCP server (PONG ok, error reply/closed port/non-redis scheme down, /db suffix ignored). `/readyz` now reports db + redis (`build_router` takes the redis URL; `apps/api/tests/readyz.rs` extended: 3 tests green live — 200 with both up, 503 db-down, 503 redis-down with db ok). Native redis 7.0.15 on 127.0.0.1:6379 is the dev stand-in (ASSUMPTION-008): installed via apt, `/etc/redis/redis.conf` carries the same cache-role flags; the systemd unit cannot start on this VM (217/USER user-namespacing), so it is started manually and must be restarted by hand after a VM reboot. Image `redis:7-alpine` loaded into docker from `.scratch/images/redis-7-alpine.tar`.

- [x] **P0-047** — Connection manager
  - **Type:** feat · **Area:** redis · **Est:** S · **Deps:** P0-046 · **Prio:** P1
  - **Spec:** deadpool-redis pool; key helper module with naming conventions (pf:{ns}:{key}) + TTL constants per retention table.
  - **AC:**
    - [x] key helper unit tested
  - **Status:** `crates/core/src/redis/` (restructured from `redis.rs` into `mod.rs` + `keys.rs`): `create_pool(url, &RedisPoolConfig::default())` — deadpool-redis 0.22.1 (redis 0.32.7), defaults 16 connections / 2 s wait+create timeouts, no idle recycle; fails fast on a malformed URL, lazy on an unreachable server (checkout reports it). `keys`: `pf:{ns}:{key}` with a `Namespace` enum (flags, usage, ratelimit, cache, pubsub, idempotency, jobs) + typed helpers (`flags_key`, `usage_key(user, day)`, `rate_limit_key`, `cache_key`, `job_events_channel`, `idempotency_key`, `job_key`); keys must be non-empty without whitespace/control (panics — programming error). TTL consts, each with its source: `TTL_FLAG_CACHE` 30 s (= `FLAG_CACHE_TTL`), `TTL_USAGE_COUNTER` 48 h (day + 24 h grace; DB keeps 400 d), `TTL_RESULT_MAX` 72 h (longest plan TTL, Business, FR-P-01). Deliberately NO consts for rate-limit windows, per-call cache TTLs, or idempotency expiry — those are set by the caller (see the module docs). Tests: 5 key-helper unit tests (AC) + 5 probe tests, and live `crates/core/tests/redis_live.rs` (3 tests: PING + SET/GET/DEL roundtrip on a unique `pf:cache:test:{uuid}` key, malformed URL fails creation, closed port fails checkout). `just deny` green on the new deps (advisories/bans/licenses/sources ok). Not wired into api/worker yet — the pool is used by the features that need it (P0-048 rate limiter first); `/readyz` keeps the client-free PING probe from P0-046.

- [x] **P0-048** — Rate-limit primitives
  - **Type:** feat · **Area:** redis · **Est:** M · **Deps:** P0-047 · **Prio:** P1
  - **Spec:** Fixed-window + token-bucket Lua scripts (atomic) per-ns limits; unit + integration tests race-safe under 50 concurrent.
  - **AC:**
    - [x] no over-admission under race
    - [x] scripts tested
  - **Status:** `crates/core/src/redis/ratelimit.rs` + `fixed_window.lua` / `token_bucket.lua` (loaded via `include_str!`, `redis::Script` = EVALSHA with EVAL fallback; needed the `script` feature on deadpool-redis, which forwards to `redis/script`). `FixedWindow::new(limit, window)` — INCR + conditional EXPIRE in one script, returns `{count, ttl}`; `TokenBucket::new(capacity, refill_per_sec)` — HMGET/HSET refill + deduct in one script, returns `{allowed, tokens as string}` (string because Redis cannot return a fractional integer reply); bucket TTL = ceil(capacity/refill) + 60 s, 24 h when refill is 0. Keys via `keys::rate_limit_key` (`pf:ratelimit:{scope}:{id}`). Both return `RateLimit { allowed, remaining, reset_in }`. Primitives only — limits are per-plan config (open question #17, Phase 5 tuning) and the 429 + `X-RateLimit-*` HTTP layer lands with Phase 1 auth. Tests: 4 unit tests (config validation, bucket TTL, Lua-source sanity) + live `crates/core/tests/ratelimit_live.rs` (5 tests: fixed-window admit/deny + per-id independence, token-bucket burst + 1.1 s refill, both 50-way races admit exactly the limit/capacity — 10 and 5 — re-run 3x stable, bad token count rejected). `just verify` needed one retry for FLAKY-001 (shutdown test).

- [x] **P0-049** — Cache helper
  - **Type:** feat · **Area:** redis · **Est:** M · **Deps:** P0-048 · **Prio:** P1
  - **Spec:** get_or_set with TTL + singleflight (local mutex) to prevent stampede; invalidation helper; hit/miss metrics.
  - **AC:**
    - [x] stampede test green
    - [x] metrics emitted
  - **Status:** `crates/core/src/redis/cache.rs`: `Cache::new(pool, &metrics)` — `get_or_set(kind, key, ttl, loader)` (values are JSON; `ttl` >= 1 s; fast path, then per-key singleflight with a re-check inside the lock so waiters hit instead of loading; loader failures are NOT cached; corrupt cached values count as a miss and are overwritten) + `invalidate(key)` (DEL). `CacheMetrics` in `crates/core/src/metrics.rs`: `detag_cache_hits_total{kind}` / `detag_cache_misses_total{kind}` (a miss is one load, not one waiter — waiters that find the value filled count a hit); `just dashboards-check` still OK. Tests: 2 singleflight unit tests (one key serializes — max concurrency 1; different keys run in parallel — proven by a started-flag, not timing) + live `crates/core/tests/cache_live.rs` (6 tests: miss→hit with metric assertions via `gather()`, TTL expiry, invalidate, corrupt entry, stampede — 50 concurrent callers on a cold key run the loader exactly once, stable over 3 runs — loader failure not cached). Design note caught by testing: the singleflight primitive only serializes; dedup comes from the caller's re-check inside the lock (a Go-style result-sharing singleflight would need `T: Clone`).

- [x] **P0-050** — SSE pubsub channel design
  - **Type:** feat · **Area:** redis · **Est:** S · **Deps:** P0-049 · **Prio:** P1
  - **Spec:** Redis pubsub channel per job id; publisher helper + subscriber stream; bounded channel, drop+log on slow consumer.
  - **AC:**
    - [x] latency p95 <= 500ms in test
    - [x] slow consumer drops safely
  - **Status:** `crates/core/src/redis/pubsub.rs`: `JobEvent` (adjacently-tagged enum — Progress/Stage/Done/Failed; the wire JSON is exactly the api spec's SSE payload shape `{"event":"stage","data":{...}}`), `publish(pool, event)` (pooled PUBLISH, returns receiver count), `subscribe(redis_url, job_id, capacity)` → `JobEventStream` (implements `Stream` + `next_event()`). The subscriber takes the URL, not the pool: a subscription holds a dedicated connection (`Client::get_async_pubsub().split()`) for its lifetime — holding a pooled connection per SSE client would starve the pool; the sink half is kept alive in the pump task. Bounded channel (`SUBSCRIBER_BUFFER` = 64): the pump uses `try_send` — on Full it drops the event, bumps `dropped_count()`, and logs a warning; malformed messages are logged and skipped, never killing the stream. Tests: 2 unit tests (wire shape matches the api spec incl. round-trip, capacity 0 rejected without connecting) + live `crates/core/tests/pubsub_live.rs` (3 tests: roundtrip + channel isolation, latency p95 over 100 events — asserted <= 500 ms, the 100-event run finishes in ~0.03 s; slow consumer — capacity 1, burst of 30, 29 dropped, first event delivered FIFO, stream stays alive for a fresh event). The 15 s SSE heartbeat and the SSE framing are the HTTP layer's job (Phase 1/4), not this module; the durable log is the `job_events` table — a late subscriber sees no history. Bugs caught during this ticket: the latency test first used the 64-buffer for 100 events (36 dropped — test design, fixed with a 128 buffer) and measured the timestamp before the await (clippy's needless-range-loop flagged the loop while the timestamp was fixed).

- [x] **P0-051** — Redis integration tests
  - **Type:** test · **Area:** redis · **Est:** S · **Deps:** P0-050 · **Prio:** P1
  - **Spec:** testcontainers redis: primitives under concurrency, pubsub latency, failover error mapping to typed errors.
  - **AC:**
    - [x] suite green
  - **Status:** `crates/core/tests/redis_integration.rs` (6 tests, 3x stable): mixed 30-task workload on one pool (rate-limit + cache + pubsub, loader once per key, every event on its own channel), pubsub p95 ≤ 500 ms under 20-task churn, failover of all three primitives + health probe to typed `AppError::CacheUnavailable` (PF-5003, 503, PII-safe body), recovery after failover. New variant + `From` impls in `crates/core/src/error.rs` (exhaustive taxonomy tests now 34 variants; ADR-0018). Deviation logged as ASSUMPTION-009: no testcontainers on this VM (DOCKER-001) — suite runs against native redis per ASSUMPTION-008, same as P0-046..P0-050. Also restored the dev services this run (ASSUMPTION-010): redis-server 7.0.15 + postgresql-16 were gone after a VM reset; reinstalled via apt, redis started manually with dev flags, pg cluster started, `pf` role (CREATEDB) + `pf` db created. `just verify` green (1 retry for FLAKY-001 only at baseline; the P0-051 run went green on attempt 1).

- [x] **P0-052** — Redis ops notes
  - **Type:** docs · **Area:** redis · **Est:** S · **Deps:** P0-051 · **Prio:** P1
  - **Spec:** Key naming table, TTL conventions, memory expectations, degraded-mode behavior per NFR-13; in runbook.
  - **AC:**
    - [x] runbook section committed
  - **Status:** `## Redis` section added to `docs/06-DELIVERY/03-deploy-runbook.md` (before Observability): key-naming table (7 namespaces, exact `pf:{ns}:{key}` patterns, typed helpers from `crates/core/src/redis/keys.rs`), TTL table (30 s flags / 48 h usage / 72 h result ceiling / per-window buckets / per-caller cache TTL / idempotency `expires_at` / ephemeral pubsub), memory expectations (no persistence by design, 256 MB `allkeys-lru` dev cap, everything re-derivable, eviction-rate signal), degraded mode per NFR-13 — honestly split into implemented (PF-5003 503, read-through cache, flags fail-closed to OFF) vs not-yet (`Retry-After` header lands with job creation; `DetagRedisDown` needs redis_exporter; `/readyz` pending — `redis_health` probe exists), plus operating commands (ping, INFO memory, SCAN per namespace, safe FLUSHDB caveat, restart procedure, Redis-down playbook). Cross-check caught and fixed two wrong helper names (`pubsub_channel`/`job_status_key` → `job_events_channel`/`job_key`). `just verify` green on attempt 3 (attempts 1–2 failed on known FLAKY-001 only).

---

## E0.6 — Storage (area: storage)

- [x] **P0-053** — StorageProvider trait
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Trait: presign_put_part/get, multipart init/complete/abort, head, delete, list_by_prefix; S3 impl via aws-sdk-s3; MinIO target for dev.
  - **AC:**
    - [x] trait + impl compile
    - [x] unit-tested with fixtures
  - **Status:** `crates/storage` now holds the real trait (`src/provider.rs`: `StorageProvider` with native `async fn`, matching the `FlagStore` house style; `StorageError` — `NotFound`/`Invalid`/`InvalidConfig`/`Backend` with PII-free Display, provider message kept as source for logs; shared `validate_key`/`validate_part_number`/`validate_expiry`/`validate_upload_id` helpers). `src/s3.rs`: `S3Storage` via aws-sdk-s3 1.155.0 (static credentials, path-style URLs, `force_path_style(true)` so MinIO and S3 share one code path; `NoSuchUpload` → idempotent abort / `Invalid` on complete; head 404 → `NotFound`; list pages with `continuation_token`). `src/memory.rs`: in-memory fake modelling the control plane only (no bytes stored; `memory://detag.test/` presigned URLs; `complete` derives the ETag from part ETags) for API/worker unit tests. Tests: 13 unit (incl. presign URL-shape tests that run fully offline — presigning is local HMAC signing) + 4 contract tests in `tests/storage_contract.rs` run against the fake; the same contract must pass unchanged against any conforming implementation (incl. a future live-MinIO harness). `aws-sdk-s3` + `aws-smithy-types` added as workspace deps; `cargo search` confirmed the sparse index works through the proxy (the earlier `curl` 403 was the crates.io API path, not cargo). Follow-up (not in this ticket's AC): `StorageError` → `AppError` mapping lands with the first API caller (P0-055). `just verify` green on attempt 2 (attempt 1 red — the failing test was not captured before the retry overwrote the log; no storage test failed in the green run, and FLAKY-001 is the only known flaky test).

- [x] **P0-054** — MinIO in compose
  - **Type:** chore · **Area:** storage · **Est:** S · **Deps:** P0-053 · **Prio:** P1
  - **Spec:** minio service + mc bootstrap bucket creation, private policy; console port dev-only.
  - **AC:**
    - [x] bucket private by default
  - **Status:** `ops/compose.minio.yml` (pattern of compose.db/redis.yml: `network_mode: host` per ADR-0014, named volume `detag_miniodata` for /data, healthcheck `curl -f http://localhost:9000/minio/health/live`): `minio` service (`server /data --console-address :9001`, console dev-only — this is the dev file; prod must not expose it) + one-shot `bootstrap` (`minio/mc`, `depends_on` healthy, `restart: "no"`, bounded 60 s wait, `mc mb --ignore-existing` then explicit `mc anonymous set none`). All bucket/credential values come from `PF_S3_*` env interpolation; no literal secrets. Images are `:latest` on purpose — MinIO tags are dated RELEASE tags and none could be verified from this VM (Docker Hub API blocked by the egress proxy); pin to a dated RELEASE tag once CI can pull. AC verified statically, not live: `scripts/check_minio_policy.py` (YAML parse; fails on any public policy, missing `anonymous set none`, hardcoded bucket/credentials, `ports:` mappings, missing volume/healthcheck/console) passes on the file, with 8 unit tests in `scripts/test_check_minio_policy.py` proving it catches each regression; full script suite 60 tests green. `just compose-minio-check` added (python check + `docker compose config --quiet`, mirroring db/redis). Live `up --wait` + anonymous-access probe deferred to CI/E0.9: DOCKER-001 amended — the VM reset also removed Docker entirely (no CLI/daemon; compose-db/redis-check fail with 127 here), and containers could not start here even when docker existed. `just verify` green on attempt 1.

- [x] **P0-055** — Presigned flows
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P0-054 · **Prio:** P1
  - **Spec:** PUT part URLs (exp configurable), GET result URLs (per-plan TTL from FR-J-09), single-use where supported; expiry < token TTL guards.
  - **AC:**
    - [x] expiry behavior tested
  - **Status:** `crates/storage/src/policy.rs`: `PresignPolicy` (defaults: part 600 s / token 900 s / results 6 h·24 h·72 h) + `PlanTier` parsed from `plans.code` (`free`/`pro`/`business`; unknown codes rejected, never defaulted). PUT part URLs: expiry configurable via new `PF_UPLOAD_PART_URL_TTL_SECS` (Settings field + `.env.example` + runbook env table + config-reference row; `just env-check` green, snapshot test updated) and guarded strictly below the token TTL — 900 s is refused at construction, 600 s accepted. GET result URLs: per-plan TTL from FR-J-09, explicitly exempt from the token-TTL guard (they are minted fresh per authenticated request and must outlive a 15-min token) and capped by the plan TTL (Free 7 h refused) and the 7-day SigV4 cap. Single-use: determined NOT supported — SigV4 has no such mechanism; `SINGLE_USE_SUPPORTED = false` (const-asserted) with compensating controls documented (short TTLs, one key per URL, upload ids die after complete/abort). Tests: 7 policy unit tests + a wiring test proving the policy's numbers are what the real S3 impl signs (`X-Amz-Expires=600` part / `21600` Free result, offline). `just verify` green on attempt 1.

- [x] **P0-056** — Key scheme + PII-free keys
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P0-055 · **Prio:** P1
  - **Spec:** ADR-0011 layout {env}/{yyyy-mm}/{job_id}/{artifact}.{ext}; helper unit-tested; traversal rejected.
  - **AC:**
    - [x] no PII in keys
    - [x] traversal rejected
  - **Status:** `crates/storage/src/keys.rs`: `storage_key(env, date, job_id, kind, ext)` builds `{env}/{yyyy-mm}/{job_id}/{stem}.{ext}` per ADR-0011; `ArtifactKind` covers the input plus the six `artifacts.type` values (`output|mask|preview_before|preview_after|report|batch_zip`), with fixed extensions enforced for `report.json` / `batch.zip` and caller-supplied (lowercased, shape-validated) extensions otherwise. No PII by construction: `env` must be `dev`/`staging`/`prod` (emails/usernames rejected), the date is a `NaiveDate`, the job id a `Uuid`, stems from the enum — the original filename lives in the DB row, never in the key; a property test asserts built keys are only `[a-z0-9-/._]`. Traversal rejected at construction (bad env/ext/`..`/slash/dot/whitespace/NUL) and again on read-back via `parse_storage_key`, which validates all four segments (env, `yyyy-mm` with month 01–12, UUID job id, known stem, fixed ext). Tests: 8 key tests (layout, distinct stems, lowercasing, fixed exts, traversal+PII rejection, PII-free property, parse round-trip, parse rejection of 10 malformed keys). `just verify` green on attempt 1.

- [x] **P0-057** — Multipart session mgmt
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** P0-056 · **Prio:** P1
  - **Spec:** uploads table transitions pending_parts to ready to aborted; complete verifies part count + sizes.
  - **AC:**
    - [x] state machine test green
  - **Status:** `crates/core/src/state_machine.rs` (was a placeholder): `UploadStatus` (`pending_parts`/`ready`/`aborted`, `FromStr` impl), `UploadPart`, `verify_complete` (count == row's `parts`, numbers exactly 1..=N — catches gaps/dupes/out-of-range in one check — every part non-empty, sizes sum to the row's `bytes`; failures are PF-2002 `ValidationFailed`), `transition` (complete → ready, abort → aborted, anything from ready/aborted → PF-5002 `InvalidStateTransition`). `crates/core/src/repos/uploads.rs`: dumb repo per the jobs pattern — `create` (birth state hardcoded `pending_parts`), `by_id`, `set_status_guarded` (atomic `UPDATE ... WHERE status = $from`, `None` when the guard misses so a concurrent transition cannot slip through). Composition is documented, not hidden: the Phase 3 service does fetch → `transition()` → guarded update, with the S3 `complete_multipart` call between verification and persistence (S3's 5 MiB non-final-part minimum is the backend's rule, not re-checked here). Tests: 9 state-machine unit tests + 2 live repo tests (`uploads_roundtrip_and_constraints`, `uploads_transitions_are_guarded` — double-complete/abort-after-ready/ready-after-abort all match nothing). Incidents fixed en route: (1) `just sqlx-prepare` was broken — the justfile exports `SQLX_OFFLINE=true`, so prepare's internal check ran offline, wiped `.sqlx/` and failed; restored from git, regenerated against a scratch DB (migrations applied via psql), added the 3 new query files, and fixed the recipe to set `SQLX_OFFLINE=false` with a migrated-DB prerequisite note; `just sqlx-prepare-check` now passes. (2) The dev `pf` DB had no tables after the VM reset — `just migrate` applied all 5 migrations, restoring it. `just verify` green on attempt 1.

- [x] **P0-058** — Integrity checksums
  - **Type:** feat · **Area:** storage · **Est:** M · **Deps:** P0-057 · **Prio:** P1
  - **Spec:** Client-reported part sha256; server recomputes on complete (streaming); mismatch PF-2004; final object hash stored (FR-U-05).
  - **AC:**
    - [x] mismatch rejected
    - [x] hash stored
  - **Status:** `crates/core/src/checksum.rs` (new dep `sha2 0.11`): `UploadVerifier` — constructed with the client's per-part report (empty/duplicate/malformed-hex reports rejected), `verify_part` streams one part's bytes (8 KiB buffer, never held in memory) through its part hasher and the whole-object hasher, `finish` returns the whole-object sha256 hex. Per-part mismatch → PF-2004 `PartChecksumMismatch`; unknown/repeated/missing parts and out-of-order parts → PF-2002 `ValidationFailed` (parts must arrive in ascending order — the whole hash is order-dependent; the first test run caught this: an out-of-order test failed because the whole hash came out wrong, so ordering is now enforced, not assumed). Plus `sha256_hex` / `sha256_reader` / `hash_matches` helpers. `repos/uploads.rs`: `set_sha256` stores the server-computed hash, guarded on `pending_parts` like the status transition — the stored value is the verified hash, not the creation-time claim (per-part verify is optional per the Phase 3 doc; the whole-hash verify against the claim is the service's job at complete). Tests: 7 checksum unit tests (NIST vectors, reader==one-shot on 100 KB, happy path, out-of-order rejection, PF-2004 mismatch with code assert, unknown/repeated/missing rejection, report validation) + live repo test `uploads_verified_hash_is_stored` (wrong claim replaced by the verified hash; no overwrite once ready; unknown id → None). `just sqlx-prepare` with the fixed P0-057 recipe added the 1 new query file. `just verify` green on attempt 1.

- [x] **P0-059** — Content-type sniffing
  - **Type:** feat · **Area:** storage · **Est:** S · **Deps:** P0-058 · **Prio:** P1
  - **Spec:** Magic-byte sniff on completed object (head+tail); allowlist png/jpeg/webp/mp4/mkv/webm; mismatch rejected.
  - **AC:**
    - [x] polyglot test rejected
  - **Status:** `crates/core/src/media.rs`: `sniff_media(head, tail, total_size)` — head magic decides the kind (PNG sig / JPEG `FF D8 FF`+marker / WebP `RIFF`+`WEBP` / MP4 `ftyp`@4 / EBML + DocType `matroska`/`webm`), then a kind-specific tail check (PNG must end with IEND, JPEG with `FF D9`, WebP's RIFF size must equal `total_size - 8`), then a generic tail scan for ZIP signatures (`PK\x03\x04`/`PK\x05\x06`) — the whole tail check for MP4/MKV/WebM, which have no tail marker. Unknown/truncated → PF-2001 `UnsupportedMediaType` (415). `check_claimed` compares the sniffed kind against the claimed extension (the server trusts bytes, not the client). Tests: 8 media tests — all six kinds sniff, canonical mime/ext, claimed-mismatch rejected, three polyglots rejected (PNG+ZIP, JPEG+ZIP, MP4+ZIP — the MP4 case proving the archive scan), truncated/wrong-size/EBML-without-doctype/random/empty/ZIP-first rejected, PF-2001 code+status assert. `just verify` green on attempt 1.

- [x] **P0-060** — Lifecycle TTL policy
  - **Type:** chore · **Area:** storage · **Est:** M · **Deps:** P0-059 · **Prio:** P1
  - **Spec:** MinIO ILM script + docs for S3/R2 lifecycle for outputs per plan TTL + orphan GC fallback job.
  - **AC:**
    - [x] ILM rule applied
    - [x] GC job skeleton runs
  - **Status:** Two layers, because no backend can express the per-plan TTLs in ILM (S3/MinIO/R2 lifecycle granularity is days — a 6 h / 24 h TTL cannot be written as a rule; R2 lifecycle support verified against Cloudflare docs 2026-11-12). (1) ILM ceiling: `ops/minio/lifecycle.json` — one Enabled rule, bucket-wide (`Prefix: ""`), `Expiration.Days: 3` (the 72 h Business TTL, FR-J-09) + abort incomplete multipart after 7 days; applied by the compose bootstrap via `mc ilm import` (JSON mounted read-only); same document works for S3 (`put-bucket-lifecycle-configuration`) and R2 (dashboard / `wrangler r2 bucket lifecycle add --expire-days 3`). `scripts/check_minio_lifecycle.py` gates it (12 tests: disabled/over-ceiling/prefix-scoped/duplicate/missing-expiry all rejected) and runs in `just compose-minio-check`. (2) GC job: new binary crate `apps/gc` (`detag-gc`, `--dry-run` default / `--execute`, `--orphan-grace-hours`, default 24 h): pure planner (`plan.rs`, 4 tests) + runner (`run.rs`, 2 tests against the memory fake) + `main.rs` wiring Settings + eager Postgres connect + S3Storage. Deletes expired artifacts from `artifacts.expires_at` (new `repos/artifacts.rs`: `expired_storage_keys` / `all_storage_keys`, live test `artifacts_gc_key_lookups` in a `detag_p060_artifacts` scratch DB) and orphans older than the grace; never deletes inputs (no `expires_at` — ILM is their backstop); fails closed when the DB/S3 is unreachable. Runbook gained `## Storage lifecycle & retention` (MinIO/S3/R2 commands, GC usage, scheduling note). `just gc` dry-runs. Live verification deferred to CI/E0.9 (DOCKER-001): `just gc` here exits 1 with "run failed: storage backend error" (no MinIO on this VM) — the skeleton's execute path is proven by the memory-fake tests. `just verify` green on attempt 2 (attempt 1 red on `slow_consumer_drops_safely` — logged as FLAKY-002, single occurrence, passed on retry); script suite 72 tests green.

- [x] **P0-061** — Storage integration tests
  - **Type:** test · **Area:** storage · **Est:** M · **Deps:** P0-060 · **Prio:** P1
  - **Spec:** testcontainers minio: multipart happy path, part mismatch, presign expiry, delete, key traversal; offline mocks for CI.
  - **AC:**
    - [x] suite green offline
  - **Status:** `crates/storage/tests/storage_integration.rs`: one shared suite (`run_suite`) over `&impl StorageProvider`, run offline against `MemoryStorage` (2 tests: `offline_memory_fake` runs all six cases, `live_minio_testcontainers` SKIPs without `PF_TEST_LIVE_MINIO=1`) and live against `S3Storage` on a testcontainers MinIO (`minio/minio:latest`, port 9000, `WaitFor::message_on_stdout("API: http://")`, scratch bucket `detag-test-<uuid-v7>`, removed afterwards). Cases: multipart happy path (init → presign → complete 2 parts → head → scoped list → delete), part mismatch (unknown upload id / empty parts / empty ETag / part 0 → `Invalid`, then the correct complete still works), presign expiry (`X-Amz-Expires` in the URL, 0 and >7 d rejected), delete idempotence, key traversal (6 bad keys rejected on every method, nothing created), list scoping + sort order. One genuine backend difference is designed in, not papered over: real S3 verifies part ETags against uploaded bytes, so cases get ETags through a `PartSource` — fabricated for the fake, real SDK `upload_part` calls for MinIO (5 MiB first part, S3's non-final minimum). testcontainers 0.28 added as a dev-dep (API verified against the vendored crate source; `cargo check --tests` green). Live leg logged as ASSUMPTION-011: compile-checked only here, first real run in CI (DOCKER-001). `just verify` green on attempt 1.

- [x] **P0-062** — Storage ops section
  - **Type:** docs · **Area:** storage · **Est:** S · **Deps:** P0-061 · **Prio:** P1
  - **Spec:** Buckets, keys, lifecycle, cost note, local console, pointing at R2/S3 (endpoint override).
  - **AC:**
    - [x] runbook section committed
  - **Status:** `## Storage operations` added to `docs/06-DELIVERY/03-deploy-runbook.md` (before the P0-060 lifecycle section, which it cross-references): buckets (one per env, `detag-<env>`, private always, creation is ops — the client never creates one), keys (ADR-0011 layout + the seven stems, pointers to `keys.rs`/`checksum.rs`/`media.rs`), presigned flows (TTL table pointer to `policy.rs`, single-use honestly marked unsupported), local MinIO console (`:9001`, dev-only, login + `mc` command block), cost note (MinIO $0 beyond disk; R2 Standard $0.015/GB-mo, IA $0.01, free tier 10 GB-mo + 1M Class A + 10M Class B, zero egress — Cloudflare docs checked 2026-11-12; S3 ≈$0.023/GB-mo + $0.09/GB egress after 100 GB free, marked verify-at-deploy; egress is the number to watch), and storage-down playbook (fail-closed design rule, flagged as enforced with the Phase 2/3 API error mapping — not claimed as current behaviour). All referenced paths verified to exist. `just verify` green on attempt 1.

---

## E0.7 — API skeleton (area: api)

- [x] **P0-063** — Axum server bootstrap
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** apps/api main: router, state (pool, redis, storage, settings, clock), tower layer order documented, two listeners (public 8080, metrics 8081).
  - **AC:**
    - [x] server boots
    - [x] layer order documented
  - **Status:** `crates/core/src/clock.rs` (new): `Clock` trait (`Send + Sync + Debug`, `now() -> DateTime<Utc>`), `SystemClock`, `ManualClock` (frozen, `set`/`advance`, clones share the instant) — coding standards forbid scattered `Utc::now()` in services. `apps/api/src/lib.rs`: `AppState { pool: DbPool, redis: RedisPool, storage: S3Storage, settings: Arc<Settings>, clock: Arc<dyn Clock> }` + `from_settings` (lazy pg/redis pools so a down dependency never fails boot; fail-fast only on malformed config via `AppStateError`, whose Display is a static string per failure — the redis inner error can contain a URL, so it is never echoed) + `with_clock` for tests. `build_router(state, metrics)` uses `with_state` (readyz now reads `State<AppState>`); `Metrics` stays an `Extension` (shared with the /metrics listener). `main.rs` builds the state and serves the two existing listeners (public 8080, metrics 9090). **Deviation from the spec line:** the spec says "metrics 8081", but the shipped default is 9090 (P0-026, runbook + config reference + ASSUMPTION-003 all agree) — 9090 stands; changing a documented default to match a stale spec number would break the config. Layer order is documented as an outer→inner table + principles in the lib.rs module docs (`Extension<Metrics>` outermost → `http_metrics` timer → `request_id` → routes + state). Tests: 2 clock unit tests, 5 lib tests (full-state build, frozen clock, malformed redis/S3 fail boot, error Display leaks nothing — the 4 sync ones had to become `#[tokio::test]` because `connect_lazy` needs a Tokio context), new `tests/bootstrap.rs` (real socket on :0, /healthz 200 + /readyz 200-or-503 with a well-formed readiness body), `tests/request_id.rs` updated to the new signature. The spawned-binary readyz (3) + shutdown tests still pass. `just verify` green on attempt 1.

- [x] **P0-064** — Router module pattern
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-063 · **Prio:** P1
  - **Spec:** routes/{auth,uploads,jobs,billing,admin,meta}.rs own their nests; shared extractor set; compile-time route-table test (no duplicate paths).
  - **AC:**
    - [x] route-table test green
  - **Status:** `apps/api/src/routes/` (new): `mod.rs` (`ROUTE_TABLE` 62 (method, path) entries from the API spec, `api_router()` merging the six module routers, shared `stub` handler answering 501 PF-5004 in the canonical problem+json envelope with the request id), `extractors.rs` (shared extractor set — re-exports `RequestId`, `AppState`, `State`, `Path`, `Query`; Phase 1 adds auth extractors there), `auth.rs` (16 routes: `/v1/auth/*` 7, `/v1/me/*` 6, `/v1/api-keys/*` 3), `uploads.rs` (4), `jobs.rs` (9 incl. `POST /v1/jobs/{id}/attestation`), `billing.rs` (12: plans 1, billing 4, usage 1, webhooks 2, webhook-endpoints 4), `admin.rs` (19 incl. `POST /v1/admin/abuse-reports/{id}/resolve` — spec says only "POST resolve with action" — plus public intake `POST /v1/abuse`), `meta.rs` (real handlers: `/healthz` + `/readyz`, moved from lib.rs). Hand-written `.route()` calls per module (Phase-1-ready — real handlers replace `stub` in place); nested sub-prefixes via `nest()`. New `AppError::NotImplemented` PF-5004, 501, `not_implemented`, "this endpoint is not implemented yet" (taxonomy now 35 variants; `all_variants` + `response_table` + range-match tests updated). `RequestId` extractor struct added to `request_id.rs` (middleware inserts it into extensions). Route-table test `tests/route_table.rs`: duplicate-free table assert (62), every entry probed with oneshot expecting 501 + PF-5004 body + matching request id (meta routes special-cased: /healthz 200 "ok", /readyz 200-or-503), negative 404 probes. Axum panics at router-build time on overlapping routes, so building the router IS the duplicate check; the table is the test oracle (reverse drift — route registered but missing from the table — is documented as not caught mechanically). Deviation from the spec line: the spec says "metrics 8081" — already settled at P0-063: shipped default is 9090, stands. Incidents fixed en route: (1) bare `use extractors::RequestId` in mod.rs resolves from the crate root — must be `self::extractors::`; (2) `.patch()`/`.put()` are MethodRouter methods, not the routing free functions — importing `patch`/`put` was unused; (3) admin.rs doc comment used `//!` mid-`///` block (E0753). `just verify` green on attempt 2 (attempt 1 red on known FLAKY-001 only).

- [x] **P0-065** — Middleware chain
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** P0-064 · **Prio:** P1
  - **Spec:** Trace, request-id, body limit (uploads exempt via streaming), CORS (web origin env), secure headers, error mapper; order asserted by integration test.
  - **AC:**
    - [x] order test green
  - **Status:** Chain (outer → inner, `build_router` + `routes::api_router`): `Extension<Metrics>` → request-id → CORS → secure headers → http-metrics → error mapper → routes (body limit per-module, uploads exempt). New modules: `apps/api/src/cors.rs` (`cors_layer(PF_WEB_ORIGIN)` — tower-http `CorsLayer`, single exact origin as `AllowOrigin::list`, credentials allowed, methods GET/POST/PUT/PATCH/DELETE/OPTIONS, allow-headers Content-Type/Authorization/X-Request-Id/Idempotency-Key, expose X-Request-Id + X-RateLimit-*, max-age 600 s), `secure_headers.rs` (HSTS `max-age=31536000; includeSubDomains`, nosniff, `X-Frame-Options: DENY`, CSP `default-src 'none'; frame-ancestors 'none'` — the exact table the security doc requires, asserted verbatim by the order test), `body_limit.rs` (1 MiB const `MAX_JSON_BODY_BYTES`, `RequestBodyLimitLayer`, applied per-module except uploads — upload bytes never transit the api, presigned S3 PUTs only), `error_mapper.rs` (innermost middleware: rewrites framework 400/404/413/422 to problem+json — PF-2002/PF-2010/PF-2003 — discarding the framework plain-text body, which can echo user input; 405 and 5xx deliberately untouched), `problem.rs` (`ProblemResponse` — renders a `Problem` with the real `application/problem+json` media type; the stub previously used axum `Json`, which sets `application/json` — body was right, media type was not; the stub now uses it). **Trace decision:** the ticket's "Trace" is the request-id middleware's `http_request` span (request_id/route/method + sampled completion event, OTel export via `init_tracing`) — tower-http's `TraceLayer` is NOT layered, because it would create a second span per request with the same fields minus the request id; the `trace` feature stays available in the workspace. **CORS semantics found in the vendored source:** `AllowOrigin::exact` emits the configured origin on EVERY response unconditionally — the list form was used instead, so a wrong origin gets no ACAO at all; and `HeaderValue::from_str("")` succeeds (empty is a valid header value), so empty/unparsable origins skip `allow_origin` entirely (the default empty list then sends nothing). New `PF_WEB_ORIGIN` (`Settings::web_origin`, default empty = CORS off, validated as a URL when set) with the full three-way config update (`.env.example` + runbook row + regenerated config reference + snapshot test assert; `just env-check` + `just config-table-check` green). Order test `tests/middleware_chain.rs` (8 tests): preflight has ACAO + request id but NO secure headers (pins CORS outside secure-headers, request-id outside CORS); real/stub/404/413 responses carry ACAO + the secure-header table + request id + problem+json with the right PF code; 2 MiB POST to `/v1/uploads` is 501 not 413 (exemption proven); CORS-off and wrong-origin get no ACAO. **Pre-existing drift fixed en route:** `just env-check` was red since P0-061 — `PF_TEST_LIVE_MINIO` (the storage live-leg gate) was referenced in code but in neither `.env.example` nor the runbook (verify doesn't run env-check, so it stayed red unnoticed); added it to `.env.example` (empty = skip) + a runbook row, regenerated the config table. `just verify` green on attempt 2 (attempt 1 red on known FLAKY-001 only).

- [x] **P0-066** — Health endpoints
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-065 · **Prio:** P1
  - **Spec:** /healthz (process), /readyz (db, redis, storage, models with per-check latency); compose healthchecks consume them.
  - **AC:**
    - [x] components report latency
  - **Status:** `ComponentHealth` gained `latency_ms: u64` + a `timed()` constructor (db.rs, P0-066) — every probe measures from probe entry to outcome, including the timeout path, so a slow component reports a large latency, never a missing one; a sub-ms probe honestly reports 0. `db_health` (P0-035) and `redis_health` (P0-046) both converted to `timed()`; `storage_health` is new in `crates/storage/src/health.rs` (`HeadBucket` on the configured bucket via `S3Storage::check_bucket`, 2 s timeout, detail PII-free because `StorageError`'s Display never echoes provider text); `models_health` is new in `crates/engine/src/registry.rs` (Phase 0: no model required, so an empty registry is ok with the detail saying so plainly — Phase 2 replaces the body with a real required-models check, P0-084). `meta.rs` `/readyz` runs all four probes concurrently with `tokio::join!` (~2 s worst case, not ~8 s). New deps: `detag-storage` + `detag-engine` now depend on `detag-core` for the shared `ComponentHealth` type (no cycle — core depends on neither); `detag-api` now depends on `detag-engine`. Tests: storage health 3 (fake-HTTP 200 up, S3 404 missing-bucket down, closed-port down + no credential leak), engine 1, redis/db latency assertions; `readyz.rs` up case now spins a fake S3 in-test (MinIO is down on this VM — DOCKER-001) and asserts all four components ok + `latency_ms <= 2050` on each, plus a new storage-down 503 case; `bootstrap.rs` asserts all four components + `latency_ms` present. `scripts/dev.sh` message updated (was stale: "storage/models land in P0-063"); runbook gained a "Health endpoints" section (shape, per-component meaning, curl commands, the 2 s bound) and the two stale "/readyz does not exist yet" sentences are fixed. Compose healthchecks themselves land in P0-088 (E0.9) — this ticket delivers the endpoints they consume. `just verify` green on attempt 2 (attempt 1 red on fmt + 3 readyz tests failing on a dead dev redis — the daemonized redis-server had been killed when its launching exec session was reaped; restarted detached with `setsid`, see ASSUMPTION-010). **Found en route:** the db probe takes the full 2 s timeout on an unreachable database — sqlx 0.9 `acquire` retries transient errors in a backoff loop until the deadline (verified in vendored `sqlx-core-0.9.0/src/pool/inner.rs`), so the down test asserts `latency_ms <= 2050`, not `< 2000` — see ASSUMPTION-012. A fail-fast db probe (raw TCP pre-check like redis) is a possible future optimisation.

- [x] **P0-067** — OpenAPI scaffold
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** P0-066 · **Prio:** P1
  - **Spec:** utoipa derive setup, swagger-ui at /docs (dev+stage only), error schema components; CI diffs generated spec vs committed spec.
  - **AC:**
    - [x] drift gate active
  - **Status:** utoipa 5.5.0 + utoipa-swagger-ui 9.0.2 (fetched via the VM egress proxy; workspace deps). `ToSchema` derives on `Problem`, `Readiness`, `ComponentHealth` (core). New `apps/api/src/openapi.rs`: `ApiDoc` (`info` Detag API 1.0.0, paths `/healthz` + `/readyz`, components Problem/Readiness/ComponentHealth, tag meta) — the `#[utoipa::path]` annotations live on the meta handlers (now `pub(crate)`), the `#[openapi]` derive imports their generated `__path_*` items. `/openapi.json` is a meta route, public in every environment (the console viewer P4-064 fetches it in prod too); `/docs` merges `SwaggerUi` only when `Settings::docs_enabled()` (dev/stage, never prod) — the UI's config points at `/openapi.json` without registering the route, so there is exactly one `/openapi.json` handler and no duplicate-route panic. New `PF_ENVIRONMENT` setting (dev | stage | prod, default dev, validated case-insensitively) with the full three-way config update (`.env.example` + runbook row + regenerated config reference; `just env-check` + `just config-table-check` green). Generator: `apps/api/src/bin/openapi.rs` prints `ApiDoc::openapi()`; `just openapi` writes `docs/api/openapi.json` (committed, OpenAPI 3.1.0), `just openapi-check` diffs it, and CI runs `just openapi-check` right after installing just. Tests: 1 spec unit test (paths + Problem fields + `latency_ms`), 3 integration tests (dev `/docs` redirect → UI html, `/openapi.json` shape, prod `/docs` 404 + spec still public), 2 new config tests (bad env names the var; docs in dev/stage only). **Found en route:** the var is `PF_ENVIRONMENT`, not `PF_ENV` — figment maps env vars to fields 1:1 after stripping `PF_` (`PF_ENV` → key `env`, which matches no field, so the value was silently ignored and both new tests failed until the rename). Coverage note: only the two real handlers are in the spec — the 62 stubs stay out until their endpoint tickets add `#[utoipa::path]` + a path here (documented in openapi.rs). `just verify` green on attempt 2 (attempt 1 red on fmt only).

- [x] **P0-068** — Pagination helpers
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-067 · **Prio:** P1
  - **Spec:** Cursor codec (base64 {ts,id}), limit clamp 1-100, Link header; tampered cursor returns 400.
  - **AC:**
    - [x] unit tests incl. tamper
  - **Status:** New `crates/core/src/pagination.rs` (shared helper — list endpoints in Phase 1+ consume it from core, not the api crate): `Cursor { ts: i64 millis, id: Uuid }` with base64url-no-pad JSON encode/decode (strict — bad base64, bad JSON, missing field, bad uuid all fail as `PaginationError::InvalidCursor`, mapped to `AppError::ValidationFailed` PF-2002 400); `clamp_limit` (None → 20, clamp 1..=100, 0 → 1); generic `Page<T> { items, next_cursor }` + `Page::from_window` (takes the limit+1 rows, truncates past the limit, cursor = new last row); `next_link` (RFC 8288 `<uri>; rel="next"`, preserves other query params, replaces `cursor`). Tests (7): round-trip + url-safety, tamper (flip at start/middle/end + truncated/garbage/non-JSON/missing-field/bad-uuid), 400 mapping (PF-2002), clamp table, from_window (exact page / over-full / empty), spec JSON shape (next_cursor omitted on the last page), next_link. `just verify` green on attempt 1.

- [x] **P0-069** — Body & size gates
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-068 · **Prio:** P1
  - **Spec:** Global JSON body limit 1MB; uploads route exempt (presign path only); oversize PF-2003.
  - **AC:**
    - [x] oversize rejected
  - **Status:** Already implemented by P0-065 — no new code in this ticket. The 1 MiB limit (`MAX_JSON_BODY_BYTES`, `body_limit.rs`) is applied per-module to every JSON route (auth, jobs, billing, admin, meta — `routes::api_router`), the uploads nest carries no limit layer (exempt structurally: upload bytes never transit the api, presigned S3 PUTs only), and the error mapper rewrites the limit layer's 413 to PF-2003 problem+json. Verified live in this ticket: the existing order tests prove 1 MiB+1 POST to `/v1/auth/login` → 413 + PF-2003 + request id + outer headers, and a 2 MiB POST to `/v1/uploads` → 501 (exempt, not 413); added one test proving the limit is global, not auth-only — 1 MiB+1 POST to `/v1/jobs` → 413 + PF-2003. `just verify` green on attempt 1.

- [x] **P0-070** — Auth extractor skeleton
  - **Type:** feat · **Area:** api · **Est:** M · **Deps:** P0-069 · **Prio:** P1
  - **Spec:** AuthContext extractor: bearer jwt OR api-key OR session cookie (web), role field; unimplemented paths return 501 until Phase 1 fills them.
  - **AC:**
    - [x] extractor compiles + 501 paths documented
  - **Status:** New `apps/api/src/auth.rs`: `AuthContext { credential, role, verified }` + `Credential::{Bearer,ApiKey,Session,None}` + `Role::{Anonymous,User,Admin,Support}`, re-exported from the shared extractor set (`routes/extractors.rs`). Parsing is pure and precedence-ordered: `Authorization: Bearer` first (JWT-shaped — two dots — vs API key), then the `detag_refresh` cookie (name provisional — Phase 1 owns both sides and confirms it); non-Bearer schemes ignored, empty Bearer is no credential. `Debug` on `Credential` prints the kind with `[redacted]` — the value never reaches a log, a `Debug` rendering, or an error body. Phase 0 contract (documented in the module): NOTHING IS VERIFIED — `verified` is always `false`, role always `Anonymous`, and the extractor never rejects; the 501s live in the stub handlers (every `/v1` path still returns 501 `PF-5004`, P0-064) until Phase 1 replaces them with real handlers AND this extractor with real verification (none → 401 PF-1001, bad JWT → PF-1010, expired → PF-1011). Tests (10): classification (JWT/api-key/case-insensitive/trimmed), cookie parsing, Bearer-beats-cookie, no-credential, Debug redaction (incl. the whole `AuthContext`), and the extractor end to end through a probe handler. `just verify` green on attempt 2 (attempt 1 red on known FLAKY-001 only).

- [x] **P0-071** — Integration harness
  - **Type:** test · **Area:** api · **Est:** M · **Deps:** P0-070 · **Prio:** P1
  - **Spec:** axum test client + testcontainers (pg, redis, minio) fixture builder: spawn_app, seed user, issue token, json() helper.
  - **AC:**
    - [x] used by first endpoint tests
  - **Status:** New `apps/api/tests/common/mod.rs`: `TestApp` (in-process oneshot client — the axum test client pattern, no sockets) with `get` / `post_json` / `request` + `TestResponse { status, headers, body }` with the `json()` helper; `spawn_app()` free fn; `TestFixtures` fixture builder with `from_env()` (native services here, compose in CI — creates + migrates a scratch DB `detag_api_harness`, never the dev `pf` DB, same rule as the core repo live tests) and `containers()` (postgres:16, redis:7, minio/minio via testcontainers, docker only — env-gated `PF_TEST_CONTAINERS=1`, skips here per DOCKER-001; MinIO bucket NOT created — bucket creation is an ops concern, same note as the storage live test). `seed_user` inserts via the users repo (hash is an explicit argument — no argon2 hasher exists yet); `seed_user_unique` generates a unique email + the `TEST_PASSWORD_HASH` placeholder (NOT a valid argon2 hash — a login against it fails by design). `issue_token` deliberately NOT implemented: minting a token needs the JWT signer + claims shape, both Phase 1 decisions (P1-003 login) — no fake token is minted, because a token the verifier rejects is worse than no helper; the module docs record this and Phase 1 endpoint tests will call the login endpoint instead. AC proven: the 3 OpenAPI tests (P0-067) were migrated onto the harness (`spawn_app`, `TestApp::spawn_with` for the prod case) and still pass. Harness tests (`apps/api/tests/harness.rs`, 3): spawn serves `/healthz` → `"ok"`, seed round-trips through the repo, testcontainers leg skips without the env var. Two fixes found by running the tests: the CREATE DATABASE race surfaces as `23505` on `pg_database_datname_index`, not `42P04` — the matcher now accepts both; and a shared helper module needs `#![allow(dead_code)]` (each test binary compiles it, and not every binary uses every item yet). `just verify` green on attempt 3 (attempt 1 red on known FLAKY-001 shutdown; attempt 2 red on known FLAKY-002 pubsub — second occurrence of FLAKY-002, see ISSUES.md; both passed in isolation/on retry with no code change, and P0-071 touches only api test code).

- [x] **P0-072** — Request logging sampling
  - **Type:** chore · **Area:** api · **Est:** S · **Deps:** P0-071 · **Prio:** P1
  - **Spec:** Structured access log with sampling (success 1%, errors 100%) + slow-request log (>250ms) per NFR-1.
  - **AC:**
    - [x] sampling verified
  - **Status:** P0-019 had already built the decision (`should_log`: errors ≥400 always, slow >250 ms always, 1% of fast successes by request-id hash) and a minimal `request completed` event — P0-072 completes it into the structured access log: the event now carries a `sampled` field (true only when the line exists because the 1% sample picked the request, false for errors/slow — new `is_sampled` helper + unit test), alongside `status` + `elapsed_ms`; `request_id`/`method`/`route` travel on the `http_request` span and appear on the same line (JSON formatter carries span fields — telemetry test proves it), so the line as a whole is the access log. No query string is ever logged (hygiene, P0-032). Verified end to end in new `apps/api/tests/access_log.rs` (4 tests, through the real router with a capturing subscriber, each test using fresh ids so parallel tests stay exact): 20/20 errors emit (all `sampled=false`); a known-sampled success emits with `sampled=true` while a known-unsampled success emits nothing; 5,000 fast successes land in the 10..150 band (~1%, ±5σ); the event line carries `request_id`/`method`/`route`/`status`/`elapsed_ms`/`sampled`. Slow requests cannot be driven through this router (every route answers immediately) — the slow rule stays unit-verified, noted in the test file. Runbook access-log section rewritten (P0-072): field list, the three rules, the `sampled` flag, and grep queries. `just verify` green on attempt 2 (attempt 1 red on known FLAKY-001 only).

- [x] **P0-073** — Error contract test
  - **Type:** feat · **Area:** api · **Est:** S · **Deps:** P0-072 · **Prio:** P1
  - **Spec:** Golden test asserts problem+json shape, PF-code ranges, request_id presence for 401, 415, 429, 500.
  - **AC:**
    - [x] golden shapes locked
  - **Status:** New `apps/api/tests/error_contract.rs` (5 tests): every error body is locked as an exact golden JSON — `{type, title, status, code, detail, request_id}` and nothing more — with hand-typed expected literals (the independent oracle from the api spec + taxonomy), the problem+json content type, and the PF-code range for the status (401→PF-1xxx, 415→PF-2xxx, 429→PF-3xxx, 500→PF-5xxx). Live legs through the real router: 415 (a `Json` extractor rejects a non-JSON content type) and a 404 control (PF-2010). Responder-level legs through `ProblemResponse` (the exact type handlers render through) for 401 (PF-1001/PF-1010/PF-1011), 429 (PF-3007/PF-3001) and 500 (PF-5000) — those three have no live path in Phase 0 (auth never rejects per P0-070, no rate-limit layer wired, no handler returns 500), so the file records that the Phase 1 endpoint tests must add the live legs when those paths land. Gap found and fixed by this ticket: the error mapper did NOT map 415 (only 400/404/413/422) — a wrong content type came back as plain text with no PF code — so the mapper now rewrites 415 → `UnsupportedMediaType` PF-2001 (module doc table, match arm, `needs_mapping`, unit test cases, and the lib.rs layer table updated). The 415 live leg uses a test-local Json probe behind the real request-id + mapper middleware in production order (no production route takes a JSON body yet — the stubs take only `Extension<RequestId>`). `just verify` green on attempt 1.

- [x] **P0-074** — API skeleton docs
  - **Type:** docs · **Area:** api · **Est:** S · **Deps:** P0-073 · **Prio:** P1
  - **Spec:** Route table generated into docs; how-to-add-endpoint checklist from repo-layout ownership map.
  - **AC:**
    - [x] checklist committed
  - **Status:** Route table generated, following the P0-067 `just openapi` pattern exactly: new generator bin `apps/api/src/bin/route_table.rs` prints Markdown from `detag_api::routes::ROUTE_TABLE` (62 routes: 2 live — `/healthz` + `/readyz`, the only real handlers — 60 stubs at 501 `PF-5004`); `just route-table` writes committed `docs/api/route-table.md`, `just route-table-check` diffs it, CI runs the check right after `openapi-check` (`.github/workflows/ci.yml`, local-only like the P0-067 edit). Liveness is derived, not stored — the bin docs record that it must learn liveness from a marker when Phase tickets land real handlers (the route-table test already special-cases the same two paths). Checklist committed as `docs/04-ENGINEERING/04-adding-an-endpoint.md`: ordered boxes from the ownership map (`02-repo-layout.md`, whose "New endpoint" row now points at it) — layer table first, PF-code pick, route + `ROUTE_TABLE` entry, `AuthContext`, `ProblemResponse`, body limits, service/repo split, `#[utoipa::path]` + regen, harness tests + golden error shapes, observability/hygiene (access log + metrics automatic, no PII, 429 + `Retry-After`), close-out. One claim checked, not invented: 429 + `Retry-After` is the rate-limit ticket's spec. `just verify` green on attempt 1.

---

## E0.8 — Worker skeleton (area: worker)

- [x] **P0-075** — apalis wiring
  - **Type:** feat · **Area:** worker · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** apps/worker: Redis-backed apalis workers (pipeline, cleanup, email, reconciliation queues), concurrency via env, versioned job envelope serde.
  - **AC:**
    - [x] workers start
    - [x] envelope version field present
  - **Status:** New `apps/worker/src/lib.rs` (testable lib: `envelope`, `queue_hooks`, `workers`) + `workers.rs`: one Redis-backed apalis 0.6 worker per queue at `PF_WORKER_CONCURRENCY` replicas (default 4, must be >= 1 — `Settings::worker_concurrency` + unit test), namespace `pf:queue:{queue}`, `Monitor::run_with_signal(shutdown_signal)` in `main.rs` (dead Redis is a boot error naming `PF_REDIS_URL`, never echoing it). `envelope.rs`: `JobEnvelope { version: 1, job_id, kind, payload }` with `check_version` refusing anything but v1 (4 unit tests incl. serde round-trip asserting the `version` field in JSON). Stub handler version-checks, records the dequeue via `QueueHooks`, acks; kind registry/retry/DLQ/drain land in P0-076..P0-081. Tests: `tests/workers_start.rs` (monitor starts, processes a pushed v1 envelope — dequeue metric observed — clean shutdown) and `tests/shutdown.rs` (spawned binary logs `started (queues:`, exits 0 on SIGTERM). Fix found while completing the interrupted run: the shutdown test used `tokio::process`, but the workspace tokio has no `process` feature — rewritten on `std::process` (the api shutdown test's pattern) with a reader thread + channel for stdout lines. `.env.example` + runbook table gained `PF_WORKER_CONCURRENCY`. `just verify` green on attempt 3 (attempt 1 red on known FLAKY-001 api shutdown; attempt 2 red on known FLAKY-002 pubsub slow-consumer; both passed on retry with no code change). Note: this run also restored the dev services after a VM reset (ASSUMPTION-010 — apt reinstall of redis-server + postgresql-16, `setsid` redis start, `pf` role/db recreated).

- [x] **P0-076** — Job envelope + registry
  - **Type:** feat · **Area:** worker · **Est:** S · **Deps:** P0-075 · **Prio:** P1
  - **Spec:** Typed envelope {job_id, kind, attempt, enqueued_at, params_hash}; registry maps kind to handler; unknown kind to DLQ + alert.
  - **AC:**
    - [x] unknown-kind test quarantines safely
  - **Status:** Envelope typed: `attempt` (0 at enqueue — the live attempt count is apalis-internal, P0-077 wires it into the retry policy), `enqueued_at: DateTime<Utc>`, `params_hash` (sha256 hex of the canonical payload JSON via `detag_core::checksum::sha256_hex`; canonical = `serde_json::to_string` with sorted keys, so equal payloads hash equal regardless of producer key order) + `verify_params_hash()`. `kind` stays a string on the wire so an unknown kind survives serde and reaches the registry; the typed view is new `JobKind` (`ping`, `pipeline`, `cleanup`, `email`, `reconciliation`, `Unknown(String)` — parse is case-sensitive by design). New `apps/worker/src/registry.rs`: `JobRegistry::dispatch` routes known kinds to Phase-0 stub handlers (ping acks; pipeline/cleanup/email/reconciliation log + ack until their features land) and quarantines unknown kinds — never executed, never retried (ack after record), counted on new `detag_queue_quarantined_total{queue}` (labelled by queue only — the unknown kind string is untrusted and never a metric label) and logged at error level with job_id + kind (the alert surface). `handle_envelope` now also quarantines an unsupported version or a `params_hash` mismatch the same way (same rule: a job this build cannot trust is never executed and never retried). The DLQ persistence (payload snapshot) is explicitly P0-078's job — until then the log line + counter are the quarantine record. Tests: 5 new envelope unit tests (attempt/enqueued_at, hash shape + verify, key-order stability, tamper fails, JSON round-trip), 4 registry unit tests (parse round-trip, case-sensitivity, dispatch handled/quarantined), new `tests/registry.rs` integration (the AC test: unknown kind `explode` → quarantine counter 1, dequeue 1, still 1 after a 2 s settle — no retry storm — then a valid `ping` is processed, dequeue 2, proving the worker survived; clean shutdown). `just verify` green on attempt 3 (attempts 1–2 red on known FLAKY-001 api shutdown only).

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
