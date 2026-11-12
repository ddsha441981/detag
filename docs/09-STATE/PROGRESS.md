# PROGRESS.md — living state (agent updates; owner reads)

> Agent: keep the tables true at all times. This file is the resume point of every run.

## Where we are

- **Current phase:** 0 — Foundation & Platform Baseline
- **Current ticket:** P0-095
- **Blocked:** P0-034 live ACs (healthy under compose, volume persists) — DOCKER-001; implementation committed, live verification deferred to CI/E0.9. Native postgres on :5432 is the dev fallback (ASSUMPTION-007). P0-083 size gate likewise deferred to CI (DOCKER-001, no docker daemon here); Dockerfile.api + gate implemented, AC unticked until CI runs green. P0-088 AC ("smoke enforces health") and P0-090 AC ("smoke green in CI") likewise unticked until the smoke job goes green in CI — `just smoke` + the CI job are implemented, but the workflow files can't reach GitHub yet (CI-WORKFLOWS-001). Remote CI runs the STALE workflow — local checks added since ~09:20 IST never ran remotely.
- **Last run:** 2026-11-12 (in progress — see NIGHT-REPORTS/)

## Phase status

| Phase | Title | Tickets done / total | Exit gates |
| --- | --- | --- | --- |
| 0 | Foundation & Platform Baseline | 91 / 108 | — |
| 1 | Identity & Billing | 0 / 111 | — |
| 2 | Removal Engine (images) | 0 / 102 | — |
| 3 | Pipeline & Video | 0 / 105 | — |
| 4 | Console & Admin | 0 / 102 | — |
| 5 | Hardening & Release | 0 / 101 | — |
| 6 | Provenance & Roadmap (gated research) | 0 / 43 | — |

## Health snapshot

| Signal | Value |
| --- | --- |
| `just verify` | green (fmt + clippy -D warnings + tests; smoke excluded per ADR-0017 — `just smoke` exists now, still excluded: no docker daemon here) — 336 passed, 0 failed (P0-094, attempt 1) |
| Coverage (core/engine/api) | — / — / — |
| Golden suite | — |
| Bench snapshot (image 4K p95 / video factor) | — |
| Open issues by severity | S1: 0 · S2: 0 · S3: 0 · S4: 3 (FLAKY-001 shutdown drain, FLAKY-002 pubsub slow-consumer, FLAKY-003 readyz storage-down port race — new in P0-093) |
| owner-input-needed items | 0 |

## Next-up queue (top 5)

1. P0-095 — Snapshot testing (insta)
2. P0-096 — proptest harness
3. P0-097 — Criterion benches scaffold
4. P0-098 — llvm-cov + coverage gate
5. P0-099 — Flaky policy tooling

## Bench history (append-only)

| Date | Engine | Image 4K p95 | Video 1080p factor | Golden pass rate |
| --- | --- | --- | --- | --- |
