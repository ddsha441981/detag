# Bug Triage & Fix Workflow

Bugs live in `09-STATE/ISSUES.md`. Nothing gets lost: even a one-line note becomes an entry.

## Severity matrix

| Sev | Definition | Response |
| --- | --- | --- |
| S1 | Security breach, data loss, corruption, AUP/legal gate broken, auth bypass | Stop feature work now; fix same run; owner notified in night report |
| S2 | Core broken: jobs fail, payments wrong, quality gate bypass, API contract broken | Fix this run or first thing next run; blocks phase exit |
| S3 | Degraded but workaround exists (slow path, ugly UI, missing nicety) | Backlog ticket in current phase |
| S4 | Cosmetic, copy, dev UX | Batch fix in polish epic |

## Entry schema (ISSUES.md)

```
### BUG-014 [S2] quality gate passes when mask covers 90% of image
- Found: nightly golden run 2026-.. · by: agent
- Repro: fixture img_4k_badge.png, mode auto
- Suspected: metric divisor uses full frame instead of unmasked area
- Fix: P5-xxx · Regression test: golden case + unit on metric area selection
- Status: open | in_progress | fixed(<commit>) | wontfix(ADR link)
```

## Fix workflow (the bugfix prompt enforces this)

1. **Reproduce** in a test first (failing test committed or listed). If not reproducible after 3 attempts → S3 entry `needs-more-info`, move on.
2. **Root cause** with evidence (log line, stack, metric), not vibes. One-paragraph RCA in the entry.
3. **Fix minimal**; resist drive-by refactors.
4. **Regression test** that fails without the fix, passes with it.
5. **Sweep for siblings**: same bug class elsewhere? Grep + fix or file tickets.
6. **Verify** full `just verify` + relevant suites (engine changes: golden suite).
7. Update entry to `fixed(<commit>)`; mention in night report.

## Regression hygiene

- Every fix ships with its regression test in the same commit.
- Golden suite grows by at least one case for every S2 engine bug fixed (the fixture that caught it).
- Flaky handling per `05-QUALITY/01-test-strategy.md`.

## Escalation to owner (never silently skip)

- S1 involving security/data: night report flags loudly at the top; next run verifies fix before new features.
- Anything needing destructive migration or data deletion: `needs-owner-ok` tag; staged as migration plan + dry-run script; owner decides.
