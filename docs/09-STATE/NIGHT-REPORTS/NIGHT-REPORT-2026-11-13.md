# NIGHT REPORT — 2026-11-13 (11th coordinator)

Run: resumed after the 10th coordinator was runtime-interrupted ~21:31 IST
mid-P1-025. Ended ~23:55 IST. Branch rule in force: work on `phase-1`,
merge each ticket into `main` locally (no rebase, no push — the driver syncs).

## Tickets completed

### P1-026 — Password policy + pwned check (DONE, `d67bda9`, merged as `a485ace`)
- Policy (`crates/validator/src/password.rs`): min length 12 chars, never
  trimmed; whole-password top-10k blocklist from zxcvbn's embedded ranked
  passwords dictionary (rank <= 10,000 — ADR-0021 / ASSUMPTION-014; no
  SecLists download, the owner's approval for raw.githubusercontent.com was
  pending); zxcvbn score >= 3 (the crate's own "too weak" line).
- Optional HIBP k-anonymity check (`crates/validator/src/hibp.rs` +
  `apps/api/src/services/pwned.rs`): only the first 5 SHA-1 hex chars leave
  the machine; `PF_PASSWORD_PWNED_CHECK` default off,
  `PF_PASSWORD_PWNED_URL` default `https://api.pwnedpasswords.com`;
  breached password is 422 `pwned_password`; any HIBP error fails open.
- Wired into register (before the Argon2 hash) and reset (before the token
  lookup — a pwned new password is 422 and the token survives).
- New deps: `zxcvbn = "3"`, `sha1 = "0.10"`, `reqwest = "0.12"`
  (rustls-tls). Testkit gains `serve_one_http_response` (answers every
  request with the same canned response until aborted).
- Tests: `just verify` green — **654 passed, 0 failed** (634 + 20 new);
  `just openapi-check` + `just route-table-check` green.
- Docs: api-spec password rule, security-testing P1-026 bullets, threat
  model T17 extended + update-log row, `just gen-config-table` re-ran,
  `just env-check` green.

### COMMITS-LINT-001 — commit linter rejects CI-FAIL-xxx subjects (DONE, `4a5ef84`, merged as `4733915`)
- `scripts/lint_commits.py` now accepts `CI-FAIL-<n>` / `SCHEMA-DOC-<n>`
  in the ticket position; 6 new unit tests (17 total in
  `test_lint_commits.py`); full scripts suite 148 green; CONTRIBUTING.md
  updated.
- Findings: the historical CI-fix commits ALSO fail on scope — every one
  used `fix(ci):`, but `ci` is a TYPE, not a scope (CONTRIBUTING.md maps
  CI work to scope `ops`). The linter was deliberately NOT taught a `ci`
  scope; a CI fix is `fix(ops): CI-FAIL-xxx ... (NFR-52)`. ce00f0c also
  carries no FR/NFR reference. The fix commit itself uses scope `repo`.
- Already-pushed non-compliant commits stay in history (no force-push);
  the Commits workflow goes green on the first push whose range holds only
  compliant commits.

## Commits this run (all on `phase-1`, merged to `main`)
- `d67bda9` feat(auth): P1-026 password policy + pwned check (FR-A-01)
- `a485ace` Merge branch 'phase-1' (P1-026)
- `4a5ef84` fix(repo): P0-006 accept CI-FAIL-xxx and SCHEMA-DOC-xxx ids (NFR-52)
- `4733915` Merge branch 'phase-1' (COMMITS-LINT-001)
- Plus the parent's parallel work on main (not mine): `b047d40` config-table
  fix, `97d6e2d` garage pull_policy, `8fb277b` CI-FAIL-008 Garage secret,
  `94ea57c` CI-FAIL-007 ffmpeg, `dfe50a4` merge. One merge conflict
  resolved by me: DECISIONS.md ADR-0020 (Garage, parent's) vs my ADR-0020
  (blocklist) — mine renumbered to ADR-0021, both kept.
- Amended while local-only: `58471cc` (non-compliant `docs(state):`
  message) → `ae2fd49` (`docs(docs): P0-006 ...`, NFR-52, linter-clean).

## Verification
- `just verify` green on the final tree: fmt + clippy -D warnings + 654
  tests, cold run after `cargo clean` (~27 min). `just env-check` green
  after the P1-026 merge (example=67 code=63 runbook=67).
- Per-package evidence for P1-026: validator 34 passed; `services::pwned`
  5 passed; register 12 passed; pwned 3 passed; password_reset 14 passed.

## Blockers / issues filed or hit
- **DISK-QUOTA-001 [S2, open — mitigated]**: `target/` had grown to
  361.7 GiB / 361,681 files; cargo writes failed with EDQUOT (os error
  122) and rust-lld crashed with SIGBUS in `StringTableBuilder::write`
  (the quota disguised as a linker crash — lld mmaps the output file).
  Fixed by `cargo clean`; a fresh full build is ~32G. It WILL recur as
  `target/` regrows — if a build fails with EDQUOT or a linker SIGBUS,
  `cargo clean` first, then retry. A cold verify after a clean is ~27 min.
- **CI #7 (parent's thread)**: verify failed at fixtures-check (NEW),
  smoke failed again. Fixed by the parent as CI-FAIL-007 (ffmpeg) +
  CI-FAIL-008 (Garage RPC secret must be 32-byte hex); CI #8 running on
  `dfe50a4` at end of this run. Verdict unknown to this coordinator.
- **Gitleaks false positive (fixed in P1-026)**: the blocklist test's
  public top-10k password `qwerty123456` tripped `generic-api-key` on
  `crates/validator/src/password.rs:195` and blocked the commit. Fixed
  with a `// gitleaks:allow` trailing comment (verified: `gitleaks
  protect --staged` clean). The same literal in `tests/register.rs`
  does not trigger (different context).
- **fmt/clippy catches in P1-026**: `cargo fmt` reformats in the new
  files; clippy 1.99 `field_reassign_with_default` in the pwned unit
  tests (fixed with struct-update syntax). Both fixed before verify.
- **ADR numbering collision (resolved)**: the parent's CI-FAIL-008 fix
  also took ADR-0020; the blocklist ADR is now ADR-0021.

## State at end of run
- Phase 1: **26 / 111**. Phase 0: 105 / 108 (unchanged).
- `main` = `4733915`, `phase-1` = `4a5ef84` (merged; tree clean).
- Next up: **P1-027 Argon2 params config** (spec: m=64MB t=3 p=4
  defaults, configurable, rehash-on-login upgrade path; benchmark note
  per target VPS). Caution: current Settings defaults are OWASP
  m=19456 KiB t=2 p=1 (P1-001) — P1-027's spec defaults differ; resolve
  deliberately and record it, do not silently overwrite. Then P1-028
  Mailer abstraction, P1-029 Email templates v1.
- Open issues: S1 0 · S2 1 (DISK-QUOTA-001) · S3 0 · S4 3 (FLAKY-001/002/003).
- No owner input needed from this run. No push performed (driver's job).
