# PROGRESS.md — living state (agent updates; owner reads)

> Agent: keep the tables true at all times. This file is the resume point of every run.

## Where we are

- **Current phase:** 1 — Identity & Billing
- **Current ticket:** P1-030 — Email flow tests
- **Blocked:** P0-034 live ACs (healthy under compose, volume persists) — DOCKER-001; implementation committed, live verification deferred to CI/E0.9. Native postgres on :5432 is the dev fallback (ASSUMPTION-007). P0-083 size gate likewise deferred to CI (DOCKER-001, no docker daemon here); Dockerfile.api + gate implemented, AC unticked until CI runs green. P0-088 AC ("smoke enforces health") and P0-090 AC ("smoke green in CI") likewise unticked until the smoke job goes green in CI — `just smoke` + the CI job are implemented, but the workflow files can't reach GitHub yet (CI-WORKFLOWS-001). Remote CI runs the STALE workflow — local checks added since ~09:20 IST never ran remotely.
- **Last run:** 2026-11-12 (in progress — see NIGHT-REPORTS/)

## Phase status

| Phase | Title | Tickets done / total | Exit gates |
| --- | --- | --- | --- |
| 0 | Foundation & Platform Baseline | 105 / 108 | P0-034/046/083 implemented but unverified (DOCKER-001) — live ACs + exit gates for `just dev`/`just smoke`/CI deferred to docker-capable CI |
| 1 | Identity & Billing | 29 / 111 | — |
| 2 | Removal Engine (images) | 0 / 102 | — |
| 3 | Pipeline & Video | 0 / 105 | — |
| 4 | Console & Admin | 0 / 102 | — |
| 5 | Hardening & Release | 0 / 101 | — |
| 6 | Provenance & Roadmap (gated research) | 0 / 43 | — |

## Health snapshot

| Signal | Value |
| --- | --- |
| `just verify` | green (fmt + clippy -D warnings + tests; smoke excluded per ADR-0017 — `just smoke` exists now, still excluded: no docker daemon here) — 685 passed, 0 failed (P1-029; 683 + 2 new: 1 HTML unit test + 1 snapshot test; full run ~29 min) — `just openapi-check` + `just route-table-check` + `just config-table-check` + `just env-check` + schema-docs-check + `just sqlx-prepare-check` + `just snapshots-check` green; `just compose-prod-check` red only because the 12th VM reset wiped the `docker` binary (DOCKER-001) — the CI-FAIL-009 fix itself verified statically (no `minio:` in base/prod, `garage:` present, both files parse; live render waits on CI) |
| Coverage (core/engine/api) | 96.8% / 99.1% / 92.6% (worker 83.8%) — all above NFR-50 floors (85/80/75/70); report-only until Phase 2 (`just coverage-gate` exists, OFF) — re-measured 2026-11-13 during P1-001 (`just coverage` green) |
| Golden suite | — |
| Bench snapshot (image 4K p95 / video factor) | full-job 4K mean 545 ms (scaffold chain — see Bench history; p95 not tracked yet, criterion mean/median in `crates/engine/benches/baselines/`) / video factor n/a (input-prep only; real factor needs the Phase 2 video pipeline) |
| Open issues by severity | S1: 0 · S2: 1 (DISK-QUOTA-001 — `target/` regrows toward the quota; `cargo clean` is the mitigation) · S3: 0 · S4: 3 (FLAKY-001 shutdown drain, FLAKY-002 pubsub slow-consumer, FLAKY-003 readyz storage-down port race — new in P0-093) |
| owner-input-needed items | 0 |

## Notes

- P1-006 verification (2026-11-13, 10th coordinator): the earlier "P1-006 ticked but no separate commit" note was a MISREAD — `f41354b feat(auth): P1-006 email change flow (token, alert, swap on verify)` exists as its own commit (26 files: verification service, email_change tests, `email_change_token_purpose` migration), directly followed by P1-007's own commit `a944c2b`. No history rewrite needed or done.
- 10th VM reset (~19:35 IST 2026-11-13): services restored per ASSUMPTION-010 (see its update in ISSUES.md — incl. the `.env` password-extraction trap); `just`/`cargo` need `~/.local/bin` + `~/.cargo/bin` on PATH in every exec call.
- 11th VM reset (~21:31 IST 2026-11-13): the 10th coordinator was runtime-interrupted mid-P1-025 with the whole ticket uncommitted; the 11th coordinator resumed, verified the tree, restored services the same way (binaries + data dir survived, `pf` role/db recreated, all migrations re-applied incl. the new `password_reset_token_purpose` one), and finished + committed P1-025 on main. Branch rule from P1-026: work on branch `phase-1`, merge each ticket into main locally (no rebase, no push).
- P1-026 done 2026-11-13 (11th coordinator): password policy + pwned check, `just verify` green 654 passed; verify was blocked mid-ticket by DISK-QUOTA-001 (linker SIGBUS = disk quota; `cargo clean` freed 361.7 GiB). COMMITS-LINT-001 fixed right after (linter accepts CI-FAIL-xxx/SCHEMA-DOC-xxx ids; CI fixes must use scope `ops`, not `ci`). Main has since moved via the parent's CI fixes (b047d40 config table, dfe50a4 merge incl. CI-FAIL-007 ffmpeg + CI-FAIL-008 Garage secret); the P1-026 merge (a485ace) resolved one conflict (DECISIONS.md ADR-0020 vs ADR-0021 — both kept, mine renumbered).
- P1-029 done 2026-11-14 (14th coordinator): email templates v1 — HTML parts for all 8 templates, multipart sending, insta snapshots for all 8, `just snapshots-check` extended to mailer; `just verify` green 685 passed. Found a pipeline trap: the literal `{{ token` sequence is redacted to `<redacted>` in the agent's output pipeline (both exec heredocs and write/edit calls) — it silently broke the three link templates' token links; fixed by building the bytes at runtime and verified byte-level; documented in `crates/mailer/src/templates.rs` for the next coordinator. The run was interrupted once by the 13th VM reset (~05:20 IST, mid-verify — the os-intent replay ran 8.5 min and restored nothing; services reinstalled + restored per ASSUMPTION-010, see its update in ISSUES.md).
- P1-028 done 2026-11-14 (14th coordinator, resuming the 13th's ~60% uncommitted tree — verified, finished, committed as one commit): mailer abstraction, `just verify` green 683 passed; the run was interrupted once by the 12th VM reset (~03:40 IST, mid-verify — services restored per ASSUMPTION-010, incl. two new traps: cogentco-mirror removal doesn't survive resets, and the documented `.env` password sed needs a `grep` first). CI-FAIL-009's fix commit 8fcf057 merged to main with it. CI-FAIL-010 still open/blocked (CI #8 logs zip pending from the user).
- P1-027 done 2026-11-13 (12th coordinator): Argon2 defaults m=64 MiB/t=3/p=4 + rehash-on-login, `just verify` green 659 passed; the first verify attempt failed on ONE test (`settings_defaults_are_the_owasp_values` in `apps/api/tests/register.rs` — asserted the old defaults as `19_456`, which the `19456` grep missed; renamed to `settings_defaults_are_the_p1_027_values` + fixed the register module doc's "~50 ms" hash-cost note). Bench: hash ~195 ms / verify ~192 ms at new defaults (criterion, 10 samples, build VM). Note for the next coordinator: full `just verify` now takes ~31 min (heavier hashing); run targeted binaries first when iterating.

## Next-up queue (top 5)

1. P1-030 — Email flow tests
2. P1-031 — Email send rate limits
3. P1-032 — Email ops docs
4. P1-033 — Key generation
5. CI #8 verdict (parent's thread — verify/smoke on dfe50a4; fixtures-check + smoke were red in CI #7, fixed by CI-FAIL-007/008)

## Bench history (append-only)

| Date | Engine | Image 4K p95 | Video 1080p factor | Golden pass rate |
| --- | --- | --- | --- | --- |
| 2026-11-13 | scaffold (P0-097) | 545 ms mean, full-job chain (blur proxy, not LaMa; p95 not tracked) | n/a — input-prep only | — |
