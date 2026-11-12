---
name: Bug report
about: Report a defect. Triage follows docs/06-DELIVERY/01-bug-triage.md.
title: "[BUG] "
labels: ["bug"]
---

## Summary
<!-- One line: what broke, in plain language. -->

## Severity
<!-- Pick one — definitions in docs/06-DELIVERY/01-bug-triage.md.
     S1 = security breach, data loss, auth bypass, AUP/legal gate broken (stop everything)
     S2 = core broken (jobs fail, payments wrong, quality gate bypass, API broken)
     S3 = degraded but a workaround exists
     S4 = cosmetic / copy / dev UX -->
- [ ] S1
- [ ] S2
- [ ] S3
- [ ] S4

## Ticket link
<!-- Link the phase ticket this bug blocks or came from, e.g.
     docs/07-PHASES/PHASE-2-removal-engine.md (P2-034).
     If the bug has no ticket yet, write "needs ticket" — triage creates one. -->

## Steps to reproduce
<!-- Numbered, minimal, from a clean state. Include the fixture or input
     that triggers it (file name, mode, params). -->
1.
2.
3.

## Expected vs actual
<!-- Expected: -->
<!-- Actual: -->

## Environment
<!-- Commit SHA (or `git log --oneline -1`), `just verify` status, OS, Rust version (`rustc --version`). -->
- Commit:
- `just verify`:
- OS / Rust:

## Regression test
<!-- How should this be caught forever? A failing test first (see the
     bugfix prompt, docs/08-PROMPTS/03-BUGFIX-REVIEW.md). If you already
     have one, link the test name. -->

## Logs / screenshots
<!-- Paste the failing log lines or stack trace. No secrets, no PII
     (no emails, tokens, raw IPs, or file contents). -->
