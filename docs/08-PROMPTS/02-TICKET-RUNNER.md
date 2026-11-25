# Ticket Runner Prompt (per-ticket protocol)

The master prompt invokes this protocol for every ticket. It is reproduced here as a standalone paste-able block for manual runs or one-off agent sessions (e.g., "just do ticket P2-034").

```text
Execute ONE ticket from the PureFrame kit.

INPUT: ticket id (e.g., P2-034) or "next" = highest-priority unblocked ticket in the current phase (see 09-STATE/PROGRESS.md).

STEPS
1. Read 00-AGENT-BOOT.md §3-§4 (autonomy + non-negotiables) and the ticket block in 07-PHASES/PHASE-<n>-<slug>.md. Open every doc the ticket cites.
2. Trace: name the FR/NFR ids this ticket satisfies (they go in the commit).
3. Plan: 3-6 bullet implementation plan in your head; note any deviation from spec you intend and why (goes in commit body).
4. Test first where practical:
   - Logic tickets: write failing unit tests from the AC list.
   - Endpoint tickets: write the integration test (testcontainers) from the API spec.
   - Engine tickets: add/extend a golden case from fixtures; assert metric floors.
5. Implement smallest correct change. Match 04-ENGINEERING/01-coding-standards.md.
6. Run `just verify` until green. Engine: also run the golden suite; put metrics before→after in the commit body. Schema: additive migration + update 03-ARCHITECTURE/04-data-model.md in the same commit. Config: update .env.example + runbook table.
7. Commit: `feat(area): <ticket-id> <what> (<FR-...>)` — one ticket, one commit. No drive-by refactors.
8. State: tick the checkbox in the phase file; update 09-STATE/PROGRESS.md (done count, current ticket, next-up); if the ticket revealed a bug/assumption, add the ISSUES.md entry; if a durable decision was made, append the ADR to DECISIONS.md.
9. Lint every commit before you finish: run `just lint-commits` after each commit (`python3 scripts/lint_commits.py --single <rev>` for a specific one, `--range <base>..HEAD` for the whole run) — ticket commits AND docs/note/run-end commits. `just verify` does NOT run the linter, so a green verify never proves the message is valid. A commit whose message fails the lint must never be merged to main (Commits #110 failed exactly this way: `docs(docs): 183rd coordinator run-end note` carries no ticket id — CI-FAIL-031).
10. Phase gate: when the LAST ticket of a phase completes, update README.md and CHANGELOG.md before closing the phase, and NEVER start the next phase without the owner's explicit permission — everything must be green first (all CI runs green, no open S1, lint-commits green). The owner decides when the next phase begins.

IF BLOCKED (> 30 min / 3 attempts): stop on this ticket. ISSUES.md entry (tag blocked:, what you tried), tick nothing, report in the run summary, and if invoked manually, say so instead of faking completion.

DONE MEANS: all ticket AC boxes true, verify green, every commit message lint-clean (`just lint-commits`), state files updated, commit pushed/present. Nothing else counts.
```
