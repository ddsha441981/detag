## What (ticket)
<!-- e.g. P0-004 repo hygiene files -->
<!-- Ticket link: docs/07-PHASES/PHASE-0-foundation.md (P0-004).
     One ticket per PR; the ticket checkbox + docs/09-STATE/PROGRESS.md
     update land in the same PR. -->

## Quality-gate impact
<!-- Engine changes: golden-suite metrics before → after, and whether
     thresholds moved (threshold moves need an ADR). Other changes:
     write "none". -->

## Definition of Done checklist
- [ ] Code implements the ticket spec; deviations documented in the commit body
- [ ] Tests added/updated; suite green
- [ ] `just verify` green locally (fmt, clippy `-D warnings`, tests)
- [ ] Coverage does not drop below NFR-50 floors
- [ ] If engine change: golden suite run + metrics before/after in commit body
- [ ] If schema change: additive migration + data-model doc updated in same commit
- [ ] If new config: `.env.example` + runbook table updated
- [ ] If new user-visible copy: complies with marketing-honesty rule (docs/02-REQUIREMENTS/05-compliance-aup.md §6)
- [ ] OpenAPI/DTO docs updated when API surface changed
- [ ] Ticket checkbox ticked in phase file; `docs/09-STATE/PROGRESS.md` updated
- [ ] No secrets in repo; no PII in logs
