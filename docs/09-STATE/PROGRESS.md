# PROGRESS.md — living state (agent updates; owner reads)

> Agent: keep the tables true at all times. This file is the resume point of every run.

## Where we are

- **Current phase:** 0 — Foundation & Platform Baseline
- **Current ticket:** P0-069
- **Blocked:** P0-034 live ACs (healthy under compose, volume persists) — DOCKER-001; implementation committed, live verification deferred to CI/E0.9. Native postgres on :5432 is the dev fallback (ASSUMPTION-007).
- **Last run:** 2026-11-12 (in progress — see NIGHT-REPORTS/)

## Phase status

| Phase | Title | Tickets done / total | Exit gates |
| --- | --- | --- | --- |
| 0 | Foundation & Platform Baseline | 66 / 108 | — |
| 1 | Identity & Billing | 0 / 111 | — |
| 2 | Removal Engine (images) | 0 / 102 | — |
| 3 | Pipeline & Video | 0 / 105 | — |
| 4 | Console & Admin | 0 / 102 | — |
| 5 | Hardening & Release | 0 / 101 | — |
| 6 | Provenance & Roadmap (gated research) | 0 / 43 | — |

## Health snapshot

| Signal | Value |
| --- | --- |
| `just verify` | green (fmt + clippy -D warnings + tests; smoke excluded per ADR-0017 until P0-090) |
| Coverage (core/engine/api) | — / — / — |
| Golden suite | — |
| Bench snapshot (image 4K p95 / video factor) | — |
| Open issues by severity | S1: 0 · S2: 0 · S3: 0 · S4: 1 (FLAKY-001 shutdown drain test) |
| owner-input-needed items | 0 |

## Next-up queue (top 5)

1. P0-069 — Body & size gates
2. P0-070 — Auth extractor skeleton
3. P0-071 — Integration harness
4. P0-072 — Request logging sampling
5. P0-073 — Error contract test

## Bench history (append-only)

| Date | Engine | Image 4K p95 | Video 1080p factor | Golden pass rate |
| --- | --- | --- | --- | --- |
