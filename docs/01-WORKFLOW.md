# Software Development Workflow

This kit runs a full SDLC loop, not just coding. The agent performs every role; prompt files in `08-PROMPTS/` are the role scripts.

```
 ┌────────────────────────────────────────────────────────────────────┐
 │  1 REQUIRE          2 DESIGN           3 BUILD            4 VERIFY  │
 │  02-REQUIREMENTS    03-ARCHITECTURE    07-PHASES tickets  05-QUALITY│
 │  quality policy     ADRs, API, data    08-PROMPTS/02      just verify│
 │  compliance gates   model, standards   ticket runner      gates     │
 ├────────────────────────────────────────────────────────────────────┤
 │  6 RELEASE                          5 TRIAGE & FIX                 │
 │  06-DELIVERY/02 checklist           06-DELIVERY/01 + ISSUES.md     │
 │  06-DELIVERY/03 runbook             severity S1..S4, regression    │
 └────────────────────────────────────────────────────────────────────┘
        ▲                                                    │
        └────────────── 7 RETRO (night report) ◄─────────────┘
```

## 1. Requirements (done once, kept alive)

- Source of truth: `02-REQUIREMENTS/`. The functional list has stable IDs (`FR-*`); every ticket must trace to at least one FR or NFR.
- Requirement gathering is continuous: when the agent discovers a gap or ambiguity, it appends to `02-REQUIREMENTS/06-open-questions.md` with a chosen default and keeps building against the default. No blocking questions, ever.

## 2. Design / decisions

- Architecture lives in `03-ARCHITECTURE/`. Any durable choice not already covered becomes an ADR appended to `09-STATE/DECISIONS.md` using the template at the top of that file.
- Tech-stack changes (crate swaps) require an ADR with: alternatives considered, reason, migration cost.

## 3. Build

- Ticket source: current phase file in `07-PHASES/`. Ticket IDs `P<phase>-<num>` are permanent; never renumber.
- Execution protocol: `08-PROMPTS/02-TICKET-RUNNER.md`. One ticket = one commit. Tests land with the code (TDD where practical).
- Every ticket cites at least one `FR-*` / `NFR-*` in its commit body.

## 4. Verify (per ticket, per phase, per release)

- Per ticket: `just verify` (fmt, clippy `-D warnings`, unit + integration tests, compose smoke).
- Engine tickets additionally: golden media suite (`05-QUALITY/02-quality-benchmarks.md`) — metrics must stay within thresholds or an ADR explains the accepted regression.
- Per phase: phase exit gates (listed at the top of each phase file) must pass before the next phase starts.

## 5. Triage and fix

- All defects go to `09-STATE/ISSUES.md` with severity S1–S4 per `06-DELIVERY/01-bug-triage.md`.
- S1 (security/data loss) and S2 (core broken) preempt feature work immediately, even mid-phase.
- Every fix ships with a regression test that fails without the fix.
- Bug-fixing sprints are scheduled as explicit tickets (see Phase 5 epic E5.5) so "testing + bug fixing" is part of the plan, not an afterthought.

## 6. Release

- `06-DELIVERY/02-release-checklist.md` gates any version bump.
- Deployment and operations: `06-DELIVERY/03-deploy-runbook.md` (docker-compose first; scale path documented).

## 7. Retro

- Every run ends with a night report (format in `00-AGENT-BOOT.md` §6). Retro actions become tickets at the top of the current phase file or `ISSUES.md` entries.

## Cadence

- **Nightly:** feature/fix tickets on the current phase.
- **Every ~200 tickets:** run the full golden suite, benchmark suite, dependency audit (`cargo deny`), and refresh `PROGRESS.md` summary numbers.
- **Phase boundary:** full regression (all tests + golden suite + compose smoke from clean clone), phase exit-gate review, ADR sweep.

## Ticket states

`[ ] todo → [~] in progress (annotated in phase file) → [x] done → blocked:` tag in ISSUES.md if stuck. The phase file checkbox is the canonical state; `PROGRESS.md` mirrors counts only.
