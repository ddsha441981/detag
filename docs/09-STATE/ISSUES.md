# ISSUES.md — bug & blocker backlog (agent-maintained)

> Schema per 06-DELIVERY/01-bug-triage.md. Severity first-class: S1 interrupts everything. Keep entries updated (`status: open | in_progress | fixed(<commit>) | wontfix(ADR)`). Tags: `blocked:`, `assumption:`, `needs-owner-ok:`, `flaky:`.

## Open issues

### ASSUMPTION-001 [assumption] `just dev` is a partial stack until E0.9/E0.4 land (P0-012)
- Date: 2026-11-12 · Ticket: P0-012
- Assumption: `just dev` (scripts/dev.sh) starts what exists today (api + worker via `cargo run`, /healthz wait) and reports compose deps, /readyz, migrations and seed as pending with their ticket ids — it does not fake a full stack. The Phase 0 exit gate re-verifies `just dev` against the full stack once P0-036 (migrations), P0-043 (seed) and P0-085..P0-090 (compose) land.
- Chosen default: seed step is gated on `ops/seed.sql` existing; if P0-043 provides a different seed artifact, dev.sh's gate is updated in that ticket.
- Status: open

### ASSUMPTION-002 [assumption] P0-022 ships the flag resolver + traits + in-memory store/cache; sqlx repo + Redis impl land with E0.4/E0.8 (P0-022)
- Date: 2026-11-12 · Ticket: P0-022
- Assumption: the real sqlx `FlagStore` and Redis `FlagCache` cannot be built or verified in this ticket — no sqlx/redis client in the workspace, no local Postgres/Redis, no migration runner (P0-036), and Docker Hub pulls fail (ADR-0017), so a live DB-backed test is impossible today. P0-022 therefore delivers the behaviour the ACs pin down (flip visible within TTL; invalidation test green) behind traits: `FlagResolver` (hides the source), `FlagStore` (repo interface) + `InMemoryFlagStore`, `FlagCache` + `InMemoryFlagCache`, `CachedFlagResolver` (30 s TTL, invalidate on admin edit), plus the `feature_flags` table as `migrations/0001_feature_flags.sql` (P0-036's runner applies it). The in-memory cache is test/dev only — in-proc caching breaks NFR-10; production uses Redis behind the same trait. `rollout` is stored, not yet evaluated (percentage rollouts are future work).
- Status: open

## Closed issues (keep for history; prune to last 50)

(none yet)

## Trend notes

(appended weekly by agent: opens/closes by severity, flaky count, oldest open)
