# Phase Index

> Counts below are generated. Agent updates `09-STATE/PROGRESS.md`, not this file.

| Phase | Title | Tickets | Focus |
| --- | --- | --- | --- |
| 0 | [Foundation & Platform Baseline](PHASE-0-foundation.md) | 108 | Workspace, config/errors, observability, Postgres/Redis/storage, API + worker skeletons, compose, test harness, security baseline |
| 1 | [Identity & Billing](PHASE-1-identity-billing.md) | 111 | Auth + sessions, API keys, RBAC, plans/quotas, Stripe (test) + Razorpay flag, metering, audit/abuse, attestation |
| 2 | [Removal Engine (images)](PHASE-2-removal-engine.md) | 102 | Intake, mask ops, template + text detection, tiled LaMa + fallback, format-preserving encode, SSIM/PSNR gates, CLI, golden suite |
| 3 | [Pipeline & Video](PHASE-3-pipeline-video.md) | 105 | Job state machine, SSE, idempotency, queue lanes, resumable uploads, video pipeline + VMAF, batches, webhooks, drills, perf |
| 4 | [Console & Admin](PHASE-4-console-admin.md) | 102 | Next.js console (uploader-first), results + billing UI, admin panel, public site, API DX |
| 5 | [Hardening & Release](PHASE-5-hardening-release.md) | 101 | Security sweep, privacy ops, load/soak, drills, bug-fixing sprint, release checklist to rc1 |
| 6 | [Provenance & Roadmap (gated research)](PHASE-6-provenance-roadmap.md) | 43 | C2PA read/preserve (+ gated strip), SynthID honest research, advanced roadmap |

**Total tickets: 672** (Phases 0-5 are build phases with 100+ tickets each; Phase 6 is a gated research backlog per 02-REQUIREMENTS/05-compliance-aup.md.)

## Suggested nightly order

1. Phase 0 completely before anything else (everything assumes it).
2. Phase 1 can start once Phase 0 storage/api/worker skeletons exist - but finish Phase 0 exit gates first.
3. Phase 2 is the longest pole: start template detection early within the phase.
4. Phase 3 needs Phase 2 engine traits; Phase 4 needs Phase 3 SSE/job contracts; Phase 5 needs all.
5. Phase 6 only after Phase 5 exit gates, and per the compliance gates.
