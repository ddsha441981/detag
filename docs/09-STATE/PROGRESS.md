# PROGRESS.md — living state (agent updates; owner reads)

> Agent: keep the tables true at all times. This file is the resume point of every run.

## Where we are

- **Current phase:** 1 — Identity & Billing
- **Current ticket:** P1-002 (Phase 0 ticket work done: 105/108; P0-034/046/083 implemented but unverified — DOCKER-001/CI-WORKFLOWS-001)
- **Blocked:** P0-034 live ACs (healthy under compose, volume persists) — DOCKER-001; implementation committed, live verification deferred to CI/E0.9. Native postgres on :5432 is the dev fallback (ASSUMPTION-007). P0-083 size gate likewise deferred to CI (DOCKER-001, no docker daemon here); Dockerfile.api + gate implemented, AC unticked until CI runs green. P0-088 AC ("smoke enforces health") and P0-090 AC ("smoke green in CI") likewise unticked until the smoke job goes green in CI — `just smoke` + the CI job are implemented, but the workflow files can't reach GitHub yet (CI-WORKFLOWS-001). Remote CI runs the STALE workflow — local checks added since ~09:20 IST never ran remotely.
- **Last run:** 2026-11-12 (in progress — see NIGHT-REPORTS/)

## Phase status

| Phase | Title | Tickets done / total | Exit gates |
| --- | --- | --- | --- |
| 0 | Foundation & Platform Baseline | 105 / 108 | P0-034/046/083 implemented but unverified (DOCKER-001) — live ACs + exit gates for `just dev`/`just smoke`/CI deferred to docker-capable CI |
| 1 | Identity & Billing | 1 / 111 | — |
| 2 | Removal Engine (images) | 0 / 102 | — |
| 3 | Pipeline & Video | 0 / 105 | — |
| 4 | Console & Admin | 0 / 102 | — |
| 5 | Hardening & Release | 0 / 101 | — |
| 6 | Provenance & Roadmap (gated research) | 0 / 43 | — |

## Health snapshot

| Signal | Value |
| --- | --- |
| `just verify` | green (fmt + clippy -D warnings + tests; smoke excluded per ADR-0017 — `just smoke` exists now, still excluded: no docker daemon here) — 418 passed, 0 failed (P1-001; 396 + 22 new: 6 password + 1 config + 4 validator + 3 service unit + 8 register; attempt 2 — clippy result_large_err, then green) |
| Coverage (core/engine/api) | 96.8% / 99.1% / 92.6% (worker 83.8%) — all above NFR-50 floors (85/80/75/70); report-only until Phase 2 (`just coverage-gate` exists, OFF) — re-measured 2026-11-13 during P1-001 (`just coverage` green) |
| Golden suite | — |
| Bench snapshot (image 4K p95 / video factor) | full-job 4K mean 545 ms (scaffold chain — see Bench history; p95 not tracked yet, criterion mean/median in `crates/engine/benches/baselines/`) / video factor n/a (input-prep only; real factor needs the Phase 2 video pipeline) |
| Open issues by severity | S1: 0 · S2: 0 · S3: 0 · S4: 3 (FLAKY-001 shutdown drain, FLAKY-002 pubsub slow-consumer, FLAKY-003 readyz storage-down port race — new in P0-093) |
| owner-input-needed items | 0 |

## Next-up queue (top 5)

1. P1-002 — Email verification flow (token single-use, unverified cannot create jobs)

## Bench history (append-only)

| Date | Engine | Image 4K p95 | Video 1080p factor | Golden pass rate |
| --- | --- | --- | --- | --- |
| 2026-11-13 | scaffold (P0-097) | 545 ms mean, full-job chain (blur proxy, not LaMa; p95 not tracked) | n/a — input-prep only | — |
