# Definition of Done

## Ticket DoD (every ticket, no exceptions)

- [ ] Code implements the ticket spec; deviations documented in commit body.
- [ ] Tests added/updated: unit, integration (endpoints), or golden (engine) as applicable; suite green.
- [ ] `just verify` green locally: fmt, clippy `-D warnings`, tests, compose smoke.
- [ ] Coverage does not drop below NFR-50 floors.
- [ ] If engine change: golden suite run + metrics before/after in commit body; thresholds unchanged or ADR.
- [ ] If schema change: additive migration + data-model doc updated in same commit.
- [ ] If new config: `.env.example` + runbook table updated.
- [ ] If new user-visible copy: complies with marketing-honesty rule (`05-compliance-aup.md` §6).
- [ ] OpenAPI/DTO docs updated when API surface changed.
- [ ] Ticket checkbox ticked in phase file; `PROGRESS.md` updated; conventional commit references ticket + FR/NFR.

## Phase DoD (exit gates at top of each phase file must all hold)

- All tickets `[x]` or explicitly deferred with ADR + owner-visible note.
- Full regression from clean clone: `docker compose down -v && just verify` plus golden suite.
- Phase-specific gates (e.g., Phase 2: golden metrics green; Phase 1: authz matrix tests).
- `09-STATE/PROGRESS.md` summary refreshed (counts, coverage, bench snapshot).
- Retro appended to latest night report: what to stop/start/continue next phase.

## Release DoD (v1.0.0)

- `06-DELIVERY/02-release-checklist.md` fully green except explicit owner-input items (live payments, domain, final pricing) which are listed, not silently skipped.
- Deployment rehearsed: clean VPS + `ops/compose.prod.yml` + caddy TLS + backups cron verified.
- Restore drill executed once (NFR-22).
- Load test at Free+Pro synthetic load meets NFR-1..NFR-5.
- Security suite (`05-QUALITY/03-security-testing.md`) green; no open S1/S2.
- Docs site pages render; OpenAPI viewer matches deployed API (diff CI green).
