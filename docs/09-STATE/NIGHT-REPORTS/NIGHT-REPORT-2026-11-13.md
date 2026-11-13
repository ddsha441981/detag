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

---

# NIGHT REPORT — 2026-11-13 (12th coordinator, appended)

Run: started 2026-11-13 ~23:32 IST (resumed cleanly from the 11th
coordinator — P1-026 done, tree clean, main = remote = `adc71b9`).
Ended 2026-11-14 ~01:07 IST (run spanned midnight). Branch rule in
force: work on `phase-1`, merge each ticket into `main` locally (no
rebase, no push — the driver syncs). No VM reset hit this run;
postgres/redis were up at start, no service restore needed.
`phase-1` was behind `main` at start (parent's CI fixes) — fast-forwarded
it to `adc71b9` before starting the ticket.

## Tickets completed

### P1-027 — Argon2 params config (DONE, `e561d79`, merged as `a6e57cf`)
- Defaults raised to m = 64 MiB, t = 3, p = 4 (spec) in `Settings`
  (`crates/core/src/config.rs`), `.env.example`, the runbook env table,
  and the regenerated config table (`just gen-config-table`).
  `Argon2Params::defaults()` is now the P1-027 values;
  `previous_defaults()` keeps the P1-001 OWASP values (m = 19 MiB,
  t = 2, p = 1) for the rehash tests. Validated ranges unchanged
  (m 4096..=1048576 KiB, t/p 1..=16).
- `needs_rehash()` (`crates/core/src/password.rs`): parses the stored
  PHC string and compares algorithm (Argon2id), version (V0x13), and
  m/t/p against the current policy; `Err` only on a malformed hash.
  (Two compile fixes while landing it: `PasswordHash.params` is a
  `ParamsString`, not `Params` — use `Params::try_from(&parsed)`; and
  `version` is `Option<u32>` (`Decimal` is a u32 alias), `algorithm`
  is an `Ident` — compare via `as_str()` / `as u32`.)
- Rehash-on-login (`apps/api/src/services/login.rs`): after a
  successful verify (and after the suspended check), a stale hash is
  rehashed with the just-verified password and written in the same tx
  as the session row. The hash runs BEFORE the tx (CPU-heavy — must
  not hold a pool connection); opportunistic — a hash failure logs a
  warn and the login still succeeds on the old hash. Uses the existing
  `users::set_password_hash` (same call shape as password_reset).
- Tests: 3 new `needs_rehash` unit tests (old params → true, current
  → false, single-param diff + Argon2i → true, malformed → error);
  2 new login integration tests (stale hash upgraded to
  `m=65536,t=3,p=4` and still verifies; current hash byte-identical
  after login). `just verify` green — **659 passed, 0 failed**
  (654 + 5 new).
- Bench: new criterion bench `crates/core/benches/argon2.rs` (10
  samples, 20 s window per bench — a full run takes ~9 min on this
  VM). Measured 2026-11-13: hash ~195 ms / verify ~192 ms at the new
  defaults vs ~26 ms / ~29 ms at the old ones. Runbook benchmark note
  added (### Argon2 cost on the target VPS) + ADR-0022.
- Docs: api-spec register paragraph, runbook env table, config
  reference regenerated, DECISIONS.md ADR-0022, phase file ticked,
  PROGRESS.md updated (27/111, current P1-028).

## Commits this run (all on `phase-1`, merged to `main`)
- `e561d79` feat(auth): P1-027 Argon2 params config (FR-A-01)
- `a6e57cf` Merge branch 'phase-1' (P1-027) — `--no-ff`: the plain
  merge fast-forwarded (phase-1 had been fast-forwarded to main
  first), so it was redone with `--no-ff` to keep the per-ticket
  merge commit the branch rule calls for. Both commits pass
  `scripts/lint_commits.py` (`--single` + `--range`).

## Verification
- `just verify` green on the final tree: fmt + clippy -D warnings +
  659 tests, 0 failed (~31 min — the heavier Argon2 defaults slow
  the auth suite; the first attempt failed on ONE test, see below).
  `just openapi-check` + `just route-table-check` +
  `just config-table-check` + `just env-check` green
  (example=67 code=63 runbook=67). Gitleaks clean on the commit.
- Per-binary evidence: password lib 9 passed; login 11 passed
  (incl. the 2 new rehash tests); register 12 passed.

## Blockers / issues filed or hit
- **Missed-test catch (fixed in-ticket, no ISSUES.md entry — it was
  my own grep miss, not a product bug)**: the first `just verify`
  failed on `settings_defaults_are_the_owasp_values`
  (`apps/api/tests/register.rs:316`) — it asserts the old defaults
  written as `19_456`, which the `19456` grep missed. Renamed to
  `settings_defaults_are_the_p1_027_values`, values updated, and the
  register module doc's "~50 ms" hash-cost note corrected to ~200 ms.
  Lesson: when a numeric default changes, grep BOTH `19456` and
  `19_456` spellings (and the `t=2`/`p=1` literals).
- **Verify is now slow (~31 min)**: the new Argon2 defaults add
  ~0.17 s per hash/verify across every auth test binary (login alone
  took ~105 s). Recorded in PROGRESS.md: run targeted binaries first
  when iterating; the full verify is the pre-commit gate, not the
  edit loop.
- No new S1/S2. No owner input needed from this run. No push performed
  (driver's job). CI #8 verdict still pending in the parent's thread
  (not chased — per handoff).

## State at end of run
- Phase 1: **27 / 111**. Phase 0: 105 / 108 (unchanged).
- `main` = `a6e57cf`, `phase-1` = `e561d79` (merged; tree clean).
- Next up: **P1-028 Mailer abstraction** (crates/mailer: lettre SMTP
  + outbox drain worker; retries with backoff; MailHog dev; tera
  templates), then P1-029 Email templates v1, P1-030 Email flow tests.
- Open issues: S1 0 · S2 1 (DISK-QUOTA-001) · S3 0 · S4 3 (FLAKY-001/002/003).
- `target/` is ~34 GiB (no quota pressure this run); disk 132 GiB free.
