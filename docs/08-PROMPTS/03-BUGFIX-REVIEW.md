# Bugfix & Self-Review Prompts

## A. Self-review prompt (run after every ~10 tickets, at epic boundaries, and on any engine change)

```text
Self-review the recent PureFrame work (last N commits / current epic).

1. Correctness: re-read each ticket spec vs the actual diff. Any ticket whose AC is not literally true: reopen it (untick) and fix now.
2. Quality gates: run the golden suite; confirm no metric regressed; confirm no threshold was silently changed (git log on thresholds.rs).
3. Contract: OpenAPI output vs 03-ARCHITECTURE/03-api-spec.md — drift is a bug; fix code or spec (code wins only with an ADR).
4. Data: migrations reviewed for additive-only (night history); data-model doc matches schema.
5. Security spot-check: new endpoints registered in the authz matrix; no PII in logs; error responses leak no internals.
6. Test honesty: any test weakened/renamed/deleted in this window? Justify or restore.
7. State hygiene: PROGRESS.md counts true; ISSUES.md entries have severity + repro; DECISIONS.md ADRs complete.

OUTPUT: a review table (area → finding → action taken) appended to the current night report. Anything not fixed now becomes a seeded ticket at the top of the phase file.
```

## B. Bugfix prompt (for a specific issue)

```text
Fix PureFrame issue <BUG-xxx> (or "top S2" = highest severity open issue in 09-STATE/ISSUES.md).

Follow 06-DELIVERY/01-bug-triage.md exactly:
1. REPRODUCE: write the failing test first (unit/integration/golden per bug class). No reproduction after 3 honest attempts → reclassify S3 needs-more-info and stop.
2. ROOT CAUSE: evidence (log, stack, metric, bisect) written into the ISSUES.md entry. No speculative fixes.
3. FIX MINIMAL: smallest correct change; no drive-by refactors.
4. REGRESSION TEST: the repro test passes now; sweep the codebase for the same bug class (grep-level) and fix or file siblings.
5. VERIFY: `just verify` + relevant suite (engine bug → golden suite; auth bug → authz matrix; billing bug → webhook idempotency tests).
6. CLOSE: ISSUES.md entry → fixed(<commit>); S2 engine bugs add a golden fixture; night report lists the fix.

HARD RULES: severity S1 = interrupt everything else. Never widen scope beyond the bug unless the fix is wrong without it (then ADR). Never mark fixed without the regression test in the same commit.
```

## C. Flaky-test prompt (weekly ritual)

```text
Quarantine sweep: find tests marked flaky in 09-STATE/ISSUES.md and any test that failed twice without a product change this week. For each: reproduce 5x, fix the nondeterminism (clock injection, sleep removal, testcontainers reuse, ordering), or keep `#[ignore = "flaky:<BUG-id>"]` with an owner-visible note. Target: zero quarantined tests older than 7 days.
```
