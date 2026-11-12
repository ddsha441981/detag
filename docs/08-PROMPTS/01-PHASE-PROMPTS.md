# Phase Kickoff Prompts

Use the master nightly prompt every night; when a new phase starts, append the phase line below to that night's prompt. Phase files in `07-PHASES/` carry the details; these lines set focus and cautions.

## Phase 0 — Foundation

```text
PHASE KICKOFF — PHASE 0 (Foundation). Read 07-PHASES/PHASE-0-foundation.md.
Focus: cargo workspace, config/errors, observability, Postgres+sqlx, Redis, storage trait, API skeleton, worker skeleton, compose, test harness, security baseline.
Caution: do not skip the test harness tickets to move faster — every later phase leans on them. Exit gates at the top of the phase file must be green before PHASE-1 ever starts.
```

## Phase 1 — Identity & Billing

```text
PHASE KICKOFF — PHASE 1 (Identity & Billing). Read 07-PHASES/PHASE-1-identity-billing.md.
Focus: auth (argon2id, JWT + rotating refresh), sessions, emails, API keys, RBAC, plans/entitlements, Stripe (test mode), Razorpay adapter (flagged), usage metering, audit.
Caution: security tests in 05-QUALITY/03-security-testing.md (authz matrix, rotation reuse) are part of this phase, not Phase 5. Stripe stays in TEST MODE; live keys are owner-input.
```

## Phase 2 — Removal Engine (images)

```text
PHASE KICKOFF — PHASE 2 (Removal Engine, images). Read 07-PHASES/PHASE-2-removal-engine.md.
Focus: intake sniffing, mask ops, template detection pack (synthesized only), DBNet ONNX text detection, tiled LaMa inpainting + in-house fallback, format-preserving encode, SSIM/PSNR metrics, engine CLI, golden suite calibration.
Caution: every engine ticket ends with a golden-suite run; numbers go in the commit body. Model licenses must be audited before download. No scraped watermark assets ever.
```

## Phase 3 — Pipeline & Video

```text
PHASE KICKOFF — PHASE 3 (Pipeline & Video). Read 07-PHASES/PHASE-3-pipeline-video.md.
Focus: job state machine, SSE progress, idempotency, queue lanes/backpressure, resumable uploads, video pipeline (ffprobe, keyframe detection, mask tracking, CRF<=16 encode, audio stream-copy), VMAF gate, batches, webhooks, GC.
Caution: kill-mid-job resilience (NFR-21) is tested with a drill script, not assumed. Memory budgets matter: no full-file-RAM video decode.
```

## Phase 4 — Console & Admin

```text
PHASE KICKOFF — PHASE 4 (Console & Admin). Read 07-PHASES/PHASE-4-console-admin.md.
Focus: Next.js scaffold, auth UI, staged uploader UX, before/after compare, quality report card, pricing/billing UI, admin panel (dashboard, users, jobs, flags, plans, audit, abuse, attestations), public pages (landing, docs, status, AUP).
Caution: upload-centric console only — no feature creep into heavy client logic; backend owns everything. Copy must pass the marketing-honesty rule.
```

## Phase 5 — Hardening & Release

```text
PHASE KICKOFF — PHASE 5 (Hardening & Release). Read 07-PHASES/PHASE-5-hardening-release.md.
Focus: security sweep, privacy/retention verification, load/soak, reliability drills, the explicit bug-fixing sprint over ISSUES.md, docs polish, release checklist execution.
Caution: bug sprint (E5.5) is mandatory — "testing + bug fixing" is part of the plan. Release stays at rc until owner-input items are answered; do not enable live payments yourself.
```

## Phase 6 — Provenance (research, gated)

```text
PHASE KICKOFF — PHASE 6 (Provenance & roadmap, RESEARCH ONLY). Read 07-PHASES/PHASE-6-provenance-roadmap.md and 02-REQUIREMENTS/05-compliance-aup.md.
Focus: C2PA read/verify/preserve (implementable), strip capability ONLY behind the legal gate; SynthID feasibility memo with honest findings; advanced roadmap.
Caution: 08-PROMPTS/04-RESEARCH.md governs this phase. No removal claims anywhere. Flag stays OFF; owner gate decides everything beyond preserve/verify.
```
