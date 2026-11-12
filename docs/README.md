# PureFrame — Agent Execution Kit

**PureFrame** (working title) is a SaaS product that removes watermarks from AI-generated images and videos with a strict no-quality-loss guarantee, built as a real engineering product: Rust backend, upload-only frontend, admin panel, JWT auth, pricing/billing, quality gates, and a gated roadmap for provenance marks (C2PA, SynthID).

This kit is a **self-contained instruction system for an autonomous coding agent** ("Muse"). The owner should never need to re-explain what to do. The agent reads this kit, picks the current phase, executes tickets, and writes a morning report.

---

## Owner quickstart (Hinglish)

1. Ek khaali repo bana (GitHub/GitLab) aur ye poora folder usme copy kar de (`pureframe-agent-kit/` ki jagah repo root pe rakhe ya `docs/` me — agent ko path chahiye bas).
2. Har raat so jaane se pehle sirf ek kaam karna hai: `08-PROMPTS/00-MASTER-NIGHTLY.md` ka prompt apne agent ko paste kar de. Bas.
3. Subah uth ke `09-STATE/NIGHT-REPORTS/` me latest report padh aur `09-STATE/PROGRESS.md` dekh — kitne ticket hue, kya decisions liye, kya blocked hai.
4. Agar koi decision tere input ka hai (jaise final pricing, domain, live payments enable karna) to wo `09-STATE/DECISIONS.md` me `owner-input-needed` tag ke saath likha milega. Wahi cheezein change kar dena, baaki agent khud karega.
5. Phase order fix hai: 0 → 1 → 2 → 3 → 4 → 5. Phase 6 (provenance/C2PA/SynthID) sirf research hai aur compliance gate ke bina implement nahi hoga.

Docs English me hain kyunki autonomous coding agents ke liye English technical specs sabse reliable hoti hain. Owner-level summary yahi Hinglish block hai.

---

## Kit map

| Path | Purpose | Read by |
| --- | --- | --- |
| `00-AGENT-BOOT.md` | Agent mission, autonomy charter, non-negotiable rules | Agent first |
| `01-WORKFLOW.md` | Full SDLC loop: requirements → dev → testing → bug fixing → release | Agent + owner |
| `02-REQUIREMENTS/` | Product overview, functional + non-functional requirements, quality policy, compliance/AUP, open questions | Agent |
| `03-ARCHITECTURE/` | System design, tech stack, API spec, data model, ADRs | Agent |
| `04-ENGINEERING/` | Coding standards, repo layout, definition of done | Agent |
| `05-QUALITY/` | Test strategy, quality benchmarks, security testing | Agent |
| `06-DELIVERY/` | Bug triage, release checklist, deploy runbook | Agent |
| `07-PHASES/` | PHASE-0 … PHASE-6 files; each has 100+ execution tickets (Phase 6 is a gated research backlog) | Agent (nightly driver) |
| `08-PROMPTS/` | Task prompts for every job: master nightly, per-phase, per-ticket, bugfix/review, research | Owner pastes, agent follows |
| `09-STATE/` | Living state: `PROGRESS.md`, `DECISIONS.md`, `ISSUES.md`, `NIGHT-REPORTS/` | Agent writes, owner reads |

`07-PHASES/INDEX.md` has the phase table with live ticket counts.

---

## Nightly ritual (the only thing the owner does)

- Paste `08-PROMPTS/00-MASTER-NIGHTLY.md` to the agent. Optionally set `NIGHT_BUDGET_MIN` (default 300 minutes).
- Morning: read the latest `09-STATE/NIGHT-REPORTS/NIGHT-REPORT-YYYY-MM-DD.md`.
- Anything tagged `owner-input-needed` in `09-STATE/DECISIONS.md`: answer it by editing that file. The agent picks answers up next run.

## Non-negotiables (apply like tests — build blocks if red)

1. `just verify` green: fmt + clippy `-D warnings` + all tests + compose smoke.
2. Quality gates pass: images SSIM(unmasked) >= 0.98 and PSNR >= 38 dB; video VMAF >= 93; audio stream-copy bit-exact; resolution never changed. See `02-REQUIREMENTS/04-quality-policy.md`.
3. Compliance gates pass: ownership attestation enforced, AUP enforced, provenance-strip features stay OFF until the legal-review gate in `02-REQUIREMENTS/05-compliance-aup.md` is signed off by the owner.
4. No secrets in repo, no destructive migrations at night, no PII in logs.
5. Every finished ticket: checked off in its phase file + one commit + entry in `09-STATE/PROGRESS.md`.

## What the agent decides alone vs what needs the owner

- **Agent decides alone** (logged as ADR in `09-STATE/DECISIONS.md`): library versions within constraints, internal module design, test approach, refactor timing, infra config values, bug priorities within the severity matrix.
- **Owner input required**: final pricing numbers, brand/domain, enabling live (non-test) payment keys, enabling any provenance-strip capability (Phase 6), deleting user data manually, changing plan quotas, spending money (servers, GPU, paid APIs).

## Glossary of state files

- `PROGRESS.md` — current phase, current ticket, done counts, next-up queue.
- `DECISIONS.md` — ADR log (append-only) + `owner-input-needed` queue.
- `ISSUES.md` — bug/blocker backlog with severity tags S1–S4.
- `NIGHT-REPORTS/` — one report per run: done, decisions, issues, metrics, next plan.
