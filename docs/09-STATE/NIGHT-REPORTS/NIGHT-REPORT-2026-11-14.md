# NIGHT REPORT — 2026-11-14 (14th coordinator)

Run: resumed after the 13th coordinator ended on cron timeout with
P1-028 ~60% done, all uncommitted on `phase-1`. Ended with P1-028
and P1-029 done and on `main`. Branch rule in force: work on
`phase-1`, merge each ticket into `main` locally (no rebase, no
push — the driver syncs). Two VM resets hit this run (12th ~03:40
IST, 13th ~05:20 IST), each interrupting a `just verify` mid-run;
both recovered per ASSUMPTION-010, see below.

## Tickets completed

### P1-028 — Mailer abstraction (DONE, `ad0ef57`, fast-forwarded to `main`)
- New `crates/mailer` (lettre 0.11 + tera 1): `smtp.rs`
  (`MailTransport` trait, `LettreTransport` — STARTTLS when
  `PF_SMTP_TLS`, credentials only when `PF_SMTP_USER` set, bad
  `PF_MAIL_FROM` fails boot — plus a fake-SMTP protocol test and
  `MemoryTransport` with `fail_next`), `templates.rs`
  (`TemplateEngine`, 8 templates × subject+text embedded at
  compile time), `drain.rs` (`drain_once` — claim → resolve →
  render → send → record; recipient resolved from the payload's
  `user_id` and verified against `to_hash`, `email-change-verify`
  goes to `pending_email`; backoff `base * 2^(n-1)` s capped at
  3600 s; permanent failures park at `attempts = max`), `error.rs`
  (transient/permanent split). Core:
  `email_outbox::{claim_due, mark_sent, record_failure,
  record_permanent_failure}` — `claim_due` is one atomic
  `UPDATE ... FOR UPDATE SKIP LOCKED` taking the caller's `now`
  + `users::set_pending_email`. Settings + `.env.example` +
  runbook + regenerated config table gain `PF_SMTP_TLS`,
  `PF_MAILER_MAX_ATTEMPTS` (5), `PF_MAILER_BACKOFF_BASE_SECS` (60),
  `PF_MAILER_DRAIN_INTERVAL_SECS` (30), `PF_MAILER_DRAIN_BATCH`
  (25). Worker: `mail_drain.rs` loop spawned in `main.rs` beside
  heartbeat/metrics, joined on shutdown.
- Tests: 24 new (13 mailer unit + 6 drain integration + 4
  `repos_live` outbox + 1 worker mail-drain test).
- Fixes found while finishing the 13th coordinator's tree:
  `LettreTransport` needed a manual `Debug` impl; `claim_due`'s
  DB-`now()` claim vs the drain's passed-`now` backoff disagreed,
  so the claim now takes `now`; the worker test first waited
  123 s on `server.await` because lettre pools the connection —
  the relay is now awaited only after the child exits (3.87 s).
- CI-FAIL-009's fix commit `8fcf057` (stale `minio:` override in
  compose.prod.yml) was on `phase-1` unmerged; it merged to `main`
  with P1-028 (fast-forward — `main` had not moved).

### P1-029 — Email templates v1 (DONE, `dbcbf60`, fast-forwarded to `main`)
- Real copy for all 8 templates the api enqueues: each gains a
  `.html.tera` part (email-safe table layout, inline styles,
  action button + plain-URL fallback, same facts as the text
  copy). `RenderedMail.html_body` + `OutgoingMail.html_body`;
  `LettreTransport` sends multipart/alternative (fake-SMTP test
  asserts multipart + the HTML part); the drain passes the HTML
  through.
- Snapshot tests: `crates/mailer/tests/snapshots.rs` (one snapshot
  per template: subject + text + HTML, fixed context) + 8 `.snap`
  files; `just snapshots-check` extended to the mailer suite;
  test-strategy doc updated. Honesty rule: grep clean — no
  "invisible" / "undetectable" / "SynthID" in templates or
  snapshots.
- Deviation from the spec's six names: "reset-password" is
  `password-reset`, "new-login" is `new-login-digest`, "lockout"
  is `lockout-notice`; "password-changed" and
  "verification-thanks" have no template because no api service
  enqueues them — no new templates invented.
- PIPELINE TRAP (new, documented in `templates.rs`): the token
  interpolation sequence (opening braces + the word "token") is
  redacted to `<redacted>` in this agent's output pipeline — both
  exec heredocs AND write/edit calls mangled it, silently breaking
  the token links in the three link templates. Fixed by building
  those bytes at runtime (`chr(123) * 2 + ' token }}'`) and
  verified byte-level (tag probe + hexdump); the verify-email
  snapshot now shows the real link. Future template edits must
  verify with a byte-level check, never by eyeballing output.
  Note: only `base_url` is marked `| safe` (operator config); the
  token stays plain-escaped — tokens are URL-safe, so the escaped
  and unescaped renderings are identical.

## Commits this run (all on `phase-1`, fast-forwarded to `main`)
- `ad0ef57` feat(mailer): P1-028 mailer abstraction with outbox drain (FR-A-01)
- `dbcbf60` feat(mailer): P1-029 email templates v1 with HTML + snapshots (FR-A-01)
- Both merges were fast-forwards (`main` had not moved since
  `ccdb9ed`), so there are no merge commits this run — history is
  linear: `ccdb9ed` → `8fcf057` → `ad0ef57` → `dbcbf60`.
- Commit lint passes on both (`lint_commits.py --single HEAD`);
  gitleaks clean on both (pre-commit hook).

## Verification
- `just verify` green on the final tree: fmt + clippy -D warnings
  + **685 passed, 0 failed** (~29 min; 683 after P1-028, +2 for
  P1-029: 1 HTML unit test + 1 snapshot test).
- `just openapi-check` + `just route-table-check` +
  `just config-table-check` + `just env-check` +
  `just schema-docs-check` + `just sqlx-prepare-check` +
  `just snapshots-check` all green.
- `just compose-prod-check` red ONLY because the 12th VM reset
  wiped the `docker` binary (DOCKER-001 — reinstalling docker is
  out of scope, containers could not start here anyway). The
  CI-FAIL-009 fix itself verified statically (no `minio:` in
  base/prod, `garage:` present, both files parse as YAML; live
  render waits on CI).

## VM resets this run (ASSUMPTION-010 updates in ISSUES.md)
- 12th reset (~03:40 IST): interrupted the P1-028 verify at ~9
  min (piped `tail -40` swallowed all progress — re-run with
  output to a file from then on). No os-intent replay running;
  self-installed postgres/redis. Two traps: (1) the
  cogentco-mirror removal from `ubuntu.sources` does NOT survive
  a reset — re-removed it, `apt-get update` went from crawling
  to 8 s. (2) The documented `.env` password sed prints the
  WHOLE file without a `grep` first — the `pf` role was briefly
  created with the entire `.env` as its password; fixed with
  `ALTER ROLE pf WITH PASSWORD 'pf'`. Always:
  `grep '^PF_DATABASE_URL=' .env | sed -E
  's/^PF_DATABASE_URL=postgres:\/\/pf:([^@]*)@.*$/\1/'`.
- 13th reset (~05:20 IST): interrupted the P1-029 verify at ~8
  min (7 binaries green; the log file survived). An os-intent
  replay WAS running (pid 765, `apt-get update`): apt was locked
  until it finished — wait it out, do NOT install in parallel.
  The wait took ~8.5 min (its update crawls on the cogentco
  mirror) and when it finished it had restored NOTHING (no
  binaries, no `/etc/redis`, no `/var/lib/postgresql`).
  Self-installed after; same restore (setsid redis,
  `pg_ctlcluster 16 main start`, recreate `pf` role/db with the
  `grep`-first extraction — verified `pw=[pf]` — `just migrate`,
  all 14 migrations applied cleanly).

## Open issues
- CI-FAIL-010 (S1, open, BLOCKED): CI #8 smoke failed again with
  a new/different failure (not the CI-FAIL-008 Garage secret).
  Exact error unknown — the CI #8 logs zip was requested from
  the user ~23:31 IST 2026-11-13 and is still pending. Do NOT
  guess at the cause; diagnose from the logs when they arrive.
  (One candidate to check against the logs: CI-FAIL-009's stale
  `minio:` override also breaks `docker compose config` for
  base+prod — if the smoke job renders prod compose, the same
  defect may be the smoke cause; confirm from the logs, do not
  assume.)
- DISK-QUOTA-001 (S2, open): `target/` was 82 GiB at last check
  (regrowing after the P1-026 clean); no EDQUOT hit this run.
- TESTKIT-MINIO-001 (S3, open): testcontainers live legs + local
  compose.minio.yml still reference dead MinIO images; follow-up
  ticket, not CI-blocking.
- FLAKY-001/002/003 (S4, open): known flakes; neither verify run
  this run hit them (P1-028 verify green first try after the
  reset re-run; P1-029 verify green first try after the reset
  re-run).

## What the next coordinator must know
- Current ticket: **P1-030 — Email flow tests** (outbox
  assertions: template, recipient, token shape; mailhog e2e for
  verify + reset; no real SMTP in CI). Groundwork already there:
  `verification.rs` / `password_reset.rs` api tests already assert
  outbox rows (template + payload token read-back via
  `email_outbox::unsent_for_template`); the fake-SMTP capture
  pattern exists in `crates/mailer/src/smtp.rs` tests and
  `apps/worker/tests/mail_drain.rs` — the e2e wires api enqueue
  → worker drain → fake SMTP capture → row marked sent. No real
  MailHog binary exists on this VM and docker does not work
  (DOCKER-001), so the "mailhog e2e" must be the in-process fake
  SMTP (or a documented MailHog-equivalent), not a container.
- Phase 1 is at **29/111**. Next in queue: P1-030, P1-031 (email
  send rate limits), P1-032 (email ops docs), P1-033 (key
  generation).
- `just verify` takes ~29 min now; run targeted binaries first
  when iterating, full verify before committing.
- NEVER push, fetch, set a remote, or rebase — GitHub sync is
  the driver's job. NEVER force-push.
- The `{{ token` redaction trap (see P1-029 above) applies to ANY
  file content containing a token interpolation — verify
  byte-level after writing template files.
- `target/` is regrowing toward the quota (82 GiB); if a cargo
  build fails with EDQUOT or a linker SIGBUS, `cargo clean`
  first and retry before debugging code (DISK-QUOTA-001).
- After any VM reset: check for a running os-intent replay FIRST
  (wait it out — it may restore nothing and take ~8 min), then
  self-install if needed; re-remove the cogentco mirror from
  `ubuntu.sources` every time; extract the `.env` password with
  the `grep`-first form; then `pg_ctlcluster 16 main start` +
  setsid redis + recreate `pf` role/db + `just migrate`.

---

# P1-030 — Email flow tests (15th coordinator, 2026-11-14)

## Tickets done
- **P1-030 — Email flow tests** (commit 9e4f55e,
  `test(auth): P1-030 email flow tests (FR-A-01)`, merged to main
  as 451ad6f). New `apps/api/tests/email_flow.rs` — 6 tests:
  outbox assertions for verify / reset / email-change
  (verify-to-new + alert-to-old) / lockout, plus fake-SMTP e2e
  for the verify and reset flows (the token is read from the
  DELIVERED mail, not the outbox). `detag-mailer` added as an api
  dev-dependency for the drain. Test-strategy doc gained an
  "Email flow tests (P1-030)" section.
- Phase 1 now **30/111**. Next: P1-031 (email send rate limits).

## Verify
- `just verify` green: **691 passed, 0 failed** (685 + 6 new),
  attempt 1, ~36 min. Commit linter passes on the ticket commit.
  No VM reset during this run; services were up at start.

## Findings (both fixed in the ticket, recorded for the next coordinator)
- Lettre sends both mail parts quoted-printable, so the equals
  sign in the action link's query string arrives as `=3D` — a
  naive read of the token after `token=` is shifted by two
  characters and the endpoint 401s (PF-1010). The suite
  QP-decodes the captured DATA first (`qp_decode` in the test
  file). Any future test that reads a token out of a delivered
  mail must decode first, not string-search the raw DATA.
- A password reset queues a `new-login-digest` mail, so the
  reset e2e's second drain legitimately claims 1 row (the
  digest) — only the third drain is empty. The test asserts the
  digest delivery explicitly.
- The e2e tests run on their own scratch databases (testkit
  `TestCtx`, dropped at the end) because `drain_once` claims
  every due row — on the shared harness DB one test's drain
  would mark another test's mail sent.

## Open issues
- CI-FAIL-012 (S1, open): CI #10 image-size — worker image
  200.8 MB vs the 200 MB gate. NOT fixed: the binary IS
  stripped, so the ~55 MB growth is in the runtime layer
  (likely ffmpeg/base image) — needs `docker history` layer
  inspection on a docker-capable runner; do NOT just bump the
  limit.
- CI-FAIL-010 (S1, fixed on main, unproven): the edge-network
  fix (2b6b99d) was itself broken (duplicate `networks:` key
  on mailhog — plain `yaml.safe_load` does NOT catch duplicate
  keys) and repaired (0d683e4). Waits on CI #11.
- CI-FAIL-011 (fixed, confirmed in CI #10) and CI-FAIL-013
  (fixed 78ceec8, waits on CI #11) are on main as e150a37.
- DISK-QUOTA-001 (S2, open): `target/` regrowing; no EDQUOT
  hit this run. TESTKIT-MINIO-001 (S3, open). FLAKY-001/002/003
  (S4, open): no flakes in this run's verify.

## What the next coordinator must know
- Current ticket: **P1-031 — Email send rate limits** (per-recipient
  hourly caps per template class: verify 3, reset 3, marketing 0;
  abuse log entries on trips). Then P1-032 (email ops docs),
  P1-033 (key generation).
- `just verify` takes ~36 min now; run targeted binaries first
  when iterating, full verify before committing.
- NEVER push, fetch, set a remote, or rebase — GitHub sync is
  the driver's job. NEVER force-push.
- Main is at 451ad6f (P1-030 merge). The driver owns the CI #11
  verdict on e150a37's repairs.
- The `{{ token` redaction trap (see P1-029 above) still applies
  to template files; the P1-030 QP finding is a different trap
  (delivered-mail decoding) — see above.

---

## 17th coordinator — Sat 2026-11-14 ~07:35–09:35 IST (run ended on cron timeout, NOT a failure)

Resumed the 16th coordinator's uncommitted P1-031 tree (cancelled by the runtime, not the user), verified it, merged main in, and finished three items. Phase 1: **30/111 → 32/111**.

### Done and on main (local main == phase-1 == `6d80ffc`; driver to push — NEVER pushed by this run)

- **P1-031 — Email send rate limits** (`db92b74`, state `3b597c7`): per-recipient hourly caps in the drain (verify 3 / reset 3 / marketing 0, ADR-0023). `just verify` green 707 passed (691 + 16 new: 7 rate_limit unit + 8 drain + 1 repos_live), attempt 1, ~35 min. Merged main `35d4922` into phase-1 first (`86427a2` — only conflict DECISIONS.md ADR-0021 edge-network vs ADR-0023 email caps; both kept).
- **CI-FAIL-016** (`971c394`, S1): CI #13 verify failed at compose-secrets-check — the CI-FAIL-015 fix's `minioadmin-secret-key` is not an allowed dev placeholder, so the secrets check flags it while the env check demands it match .env.example. Fix: `minioadmin` on both sides (`.env.example` + both `ops/compose.minio.yml` defaults) + regenerated config table. All four checks green locally (secrets, env, config-table, env-check; compose-check script suite 36 tests green). CI-FAIL-015's ISSUES.md entry now records the backfire; 016 waits on CI #14.
- **P1-032 — Email ops docs** (`17a0072`, state `6d80ffc`): new `docs/06-DELIVERY/05-email-ops.md` (transactional-only stance, 8 templates + caps, sender config, SPF/DKIM/DMARC placeholders with owner decision #4 pending, test addresses, outbox/audit SQL verified against the real schemas, troubleshooting incl. the P1-030 QP `=3D` note). Docs-only; config-table-check + env-check green.

### P1-033 — Key generation: CODE COMPLETE, NOT COMMITTED (next coordinator: verify green, then commit)

All work is uncommitted on branch `phase-1` (see `git status`): `crates/core/src/api_keys.rs` (generation: `pf_live_` + 24B base64url, sha256 of full string, display prefix + last4, `API_KEY_MAX_PER_USER = 10`, known scopes), `crates/core/src/repos/api_keys.rs` (create / count_live / user-row `FOR UPDATE` lock), migration `20261114030933` (unique index on `secret_hash` — lookup must be unambiguous, same call as sessions.refresh_hash), `apps/api/src/services/api_keys.rs` (DTO + Validate, tx: lock → count → cap → insert + `api_key.created` audit), `apps/api/src/routes/api_keys.rs` (POST, 201, secret shown once), openapi + route-table regenerated (POST now live; GET/DELETE still stubs), `apps/api/tests/api_keys.rs` (6 tests), `crates/core/tests/repos_live.rs` (+1 test), docs (api-spec row, data-model row + index line, test-strategy section), ASSUMPTION-015 in ISSUES.md (no plan gate, no verified-email requirement, cap denial is 409 `PF-2011` — no dedicated PF code).

- Targeted tests green: 4 core unit + 1 repos_live + 6 api integration (incl. 12-concurrent-creates → exactly 10×201 + 2×409). fmt + clippy clean; openapi-check, route-table-check, sqlx-prepare-check, schema-docs-check green.
- **Full `just verify` was still running when the cron timeout hit** (~13 min in, 6 binaries green, no failures; log `/tmp/p133-verify.log`, session `proc_3317c79c5844` — may still be running or killed by the timeout; CHECK it before re-running). Do NOT commit P1-033 until a full green verify on this tree.
- Lesson recorded in the test-strategy doc: test targets are NOT covered by `cargo sqlx prepare`, so tests must use untyped `sqlx::query` + `try_get` (or repo fns), never `query!` macros.

### Environment: 14th VM reset (~08:45 IST, mid-`cargo test`)

Os-intent replay (pid 776) ran ~23 min — waited it out, no parallel install. Binaries back but cluster down, `pf` role/db gone (fresh cluster). Restored per ASSUMPTION-010 (`pg_ctlcluster` + setsid redis, recreate `pf` role/db with the `grep`-first password extraction — verified `pw=[pf]` — `just migrate`, all 15 migrations applied incl. the new unique index). Tree survived intact. Noted in ASSUMPTION-010.

### Still open / blocked (unchanged)

- **CI-FAIL-012** (S1): worker image 200.8 MB vs 200 MB — needs `docker history` on a docker-capable runner; do NOT bump the limit.
- **CI #13 cargo-deny NEW failure + Commits #17 failure**: need the user's logs zip to diagnose (requested in main chat ~08:15 IST; not arrived).
- CI #14 (on the next push: P1-031 + CI-FAIL-016 + P1-032) will confirm 016; CI-FAIL-013 fix still unverified.
- DISK-QUOTA-001 (S2): no EDQUOT hit this run. TESTKIT-MINIO-001 (S3). FLAKY-001/002/003 (S4): no flakes in this run's verifies (P1-031 green attempt 1).

### What the next (18th) coordinator must know

- Current ticket: **P1-033 — Key generation** (code complete, uncommitted — see above). Finish: confirm/kill the stale verify process, run `just verify` to green, commit ONE commit `feat(auth): P1-033 key generation (FR-A-06)`, tick the phase file, update PROGRESS.md (Phase 1 → 33/111, current ticket P1-034), merge phase-1 into main locally. Then P1-034 (key auth extractor — the `Credential::ApiKey` arm in `apps/api/src/auth.rs`: hash lookup, scope check, throttled `last_used_at`).
- NEVER push, fetch, set a remote, or rebase — GitHub sync is the driver's job. NEVER force-push. Local main == phase-1 == `6d80ffc`; remote main was `56a9108` at this run's start (driver syncs).
- `just verify` takes ~35 min now; run targeted binaries first when iterating, full verify before committing.

---

## 21st coordinator (~15:45–17:0x IST) — P1-034 still uncommitted; FLAKY-002 second leg fixed + committed; 16th VM reset killed verify-3

### Merge main into phase-1 (done first, clean fast-forward)
- `git merge main` while on `phase-1`: fast-forward `6cfdf54` → `8a65cd4`, NO conflicts. Main's fixes now in the tree: FLAKY-001 (`e94b468`), CI-FAIL-012 (`0be1dfa`, worker ceiling 200 → 250 MB), FLAKY-002 drain loop (`8a65cd4`), CI-FAIL-017/018. P1-034 uncommitted tree untouched by the merge.

### P1-034 — Key auth extractor: CODE COMPLETE, NOT COMMITTED (next coordinator: verify green, then commit)
- The 19th coordinator's tree, resumed by the 20th, still uncommitted on `phase-1` (see `git status`, 23 entries): auth extractor arm (`Credential::ApiKey` — sha256 hash lookup, live-keys-only, owner must be `active` per ADR-0024, scope check by route from `MatchedPath`, `last_used_at` re-stamp throttled 300 s, `rate_limit_tier` rides `VerifiedApiKey`), `stub_authed` on jobs (9) + webhook-endpoints (4) stubs, 2 new `.sqlx/` query files, docs (api-spec, data-model, security-testing, threat-model T20, ADR-0024), phase file ticks P1-034 with "`just verify` green: TBD" — that TBD is the gate. Do NOT commit until a full green `just verify` on this tree.

### Verify attempts this run (all on the merged tree, P1-034 uncommitted)
- **Attempt 1 (adopted from the 20th coordinator, `.scratch/p134-verify.log`)**: 85 binaries ok, 1 FAILED — `detag-worker --test shutdown`, `worker_starts_and_exits_zero_on_sigterm`: "worker did not exit within 10 s of SIGTERM". Binaries were compiled PRE-merge. Standalone re-run: **3/3 passes in 0.26 s** — load flake under parallel verify (same family as FLAKY-001/004); P1-034 touches no worker shutdown code. No ISSUES.md entry made (first occurrence, passes standalone); watch for a repeat.
- **Attempt 2 (fresh, `.scratch/p134-verify-2.log`)**: 55 binaries ok, 1 FAILED — `detag-core --test pubsub_live`, `slow_consumer_drops_safely`, NEW signature at `pubsub_live.rs:186`: `fresh event arrives in time: Elapsed(())` — after publishing `Done`, NOTHING arrived within 5 s. This is FLAKY-002's **10th occurrence, second leg**: the 8a65cd4 drain loop assumed `Done` can never be dropped, but the pump uses `tx.try_send` and drops ANY event when the buffer is full — including `Done`. If the pump is still draining the burst when the first buffered event is consumed, a stale Progress lands in the freed buffer and the `Done` published next is dropped, so the drain loop waits forever. Root cause confirmed in `crates/core/src/redis/pubsub.rs:216` (`TrySendError::Full` → drop).

### FLAKY-002 second leg — FIXED + COMMITTED (`0a600f5`, own commit)
- Fix (test-only, `crates/core/tests/pubsub_live.rs`): after the burst, wait until `dropped_count() == BURST - 1` (29) before reading the first event — that proves the pump processed all 30 (1 buffered + 29 dropped) and has nothing pending, so `Done` cannot be dropped. The drain loop stays as belt-and-braces. ISSUES.md FLAKY-002 entry updated (10th occurrence, second-leg root cause + fix) in the same commit.
- Cross-checks: `slow_consumer_drops_safely` **20/20** standalone, full `pubsub_live` file **3/3**, `cargo fmt --check` + `cargo clippy -p detag-core --tests` clean, gitleaks clean on commit. Full `just verify` green TBD (attempt 3 below was killed by the reset; attempt 4 pending).

### Environment: 16th VM reset (~16:48 IST, mid-verify-3)
- `uptime` 4 min at 11:22 UTC; postgres/redis binaries + clusters gone entirely (fresh VM). Verify-3 died mid-`login.rs` binary (21 binaries ok, 0 failed, log `.scratch/p134-verify-3.log`).
- Restore COMPLETED 17:06 IST: `apt-get update` took 11 min 12 s (71.1 MB at 106 kB/s via proxy) then `redis-server postgresql-16` installed clean (exit 0). Services up: redis PONG, `pg_ctlcluster 16 main start` online, `pf` role/db recreated (password = recorded `pf` placeholder), `just migrate` applied all 15 migrations incl. P1-033's `20261114030933`.
- **Verify attempt 4 LAUNCHED 17:06 IST** (`.scratch/p134-verify-4.log`, bash pid 6244) on the restored services — full `just verify` on the P1-034 tree + FLAKY-002 second-leg fix. **FAILED at 31 binaries ok**: `detag-api --test readyz`, 3 of 4 failed — "api did not become healthy on port X within 10 s" (`readyz_ok_when_all_components_up`, `readyz_degraded_when_database_down`, `readyz_degraded_when_storage_down`). This is known **FLAKY-004** (parallel-startup latency; same signature as P1-033 attempt 1, which passed on retry with no code change) — readyz PASSED in attempt 2 on this same tree, and standalone re-run after the failure: **4/4 pass in 11 s**. NOT a P1-034 regression (P1-034 touches no startup/healthz code). 3 stray `detag-api` children from the failed run (pids 9462–9464) killed before re-launch.
- **Verify attempt 5 LAUNCHED 17:30 IST** (`.scratch/p134-verify-5.log`, bash pid 9812) — full `just verify`, same tree. Was in clippy phase when this run ended; expect done ~18:10 IST. If it goes green, the next coordinator commits P1-034 (see below). If the cron timeout hits first, the verify KEEPS RUNNING (backgrounded) — ADOPT it, do not re-launch blindly.

### Still open / blocked
- **P1-034 uncommitted** — needs a full green `just verify` (attempt 4, after services restore) then ONE commit `feat(auth): P1-034 ...`, phase-file TBD → real green numbers, PROGRESS.md Phase 1 → 34/111, current ticket P1-035, commit the state update, merge phase-1 into main locally (default `Merge branch ...` subject — never custom lowercase `merge:`).
- **Commits #21 still failing** at the linter unit-test step — needs the user's logs zip; NOT diagnosable from here; do not chase it.
- CI #18 (on 8a65cd4) will confirm the FLAKY-002 first-leg fix; the second-leg fix (`0a600f5`) is NOT on main yet — it lands when phase-1 merges into main after P1-034.
- Worker shutdown test: first occurrence of the 10 s SIGTERM timeout under load (see attempt 1) — no entry yet; if it recurs, log it as a FLAKY candidate.

### What the next (22nd) coordinator must know
- Current ticket: **P1-034 — Key auth extractor** (code complete, uncommitted — see above). Order: (1) services are RESTORED (redis PONG, pg online, `pf` db migrated, 15/15) — no restore needed unless another VM reset hits; (2) check verify attempt 5 (`.scratch/p134-verify-5.log`, launched 17:30 IST, bash pid 9812) — if still running, ADOPT it; if it finished green, commit P1-034 + state and merge phase-1 into main; if it failed, read the failure in the log first (flakes seen this run: worker-shutdown 10 s timeout, FLAKY-002 second leg — FIXED in 0a600f5, FLAKY-004 readyz 3/4 — re-run the failing test standalone, then re-run full verify; do NOT touch code on a first flake); (3) commit P1-034 + state, merge phase-1 into main locally; (4) then P1-035 Key rotation (POST `/v1/api-keys/{id}/rotate`, new secret, old valid 24 h overlap configurable, audited; AC: overlap window tested).
- NEVER push, fetch, set a remote, or rebase — GitHub sync is the driver's job. NEVER force-push. Local `main` == `8a65cd4`, remote `origin/main` == `8a65cd4` (driver-verified); local `phase-1` == `0a600f5` (FLAKY-002 second-leg fix on top of main + uncommitted P1-034).
- `just verify` takes ~35–45 min; run targeted binaries first when iterating, full verify before committing. Never stage build/test trees in `/tmp` (512 MB tmpfs) — use `~/workspace/detag/.scratch/`.

## 22nd coordinator (~17:32–17:43 IST, ended on cron timeout — NOT a failure)
- Adopted verify-5 (bash pid 9812, log `.scratch/p134-verify-5.log`, launched 17:30 IST by the 21st coordinator). At timeout (~17:43 IST): still running, 12:59 elapsed, 17 binaries `test result: ok`, **zero failures so far** (no FAILED/panicked lines). Full run takes ~31–35 min — expect it to finish ~18:02–18:05 IST.
- **Nothing was committed.** P1-034 tree still uncommitted on branch `phase-1` at `0a600f5` (24 status entries at last check: 22 modified + 2 untracked `.sqlx/` files — the driver counted 23 modified; one entry differs, re-check with `git status` before committing).
- **Next coordinator (23rd):** 1) check verify-5's `EXIT=` line in `.scratch/p134-verify-5.log`. 2) If green: commit P1-034 as ONE commit (`feat(auth): P1-034 key auth extractor` per ticket-runner convention), update PROGRESS.md (Phase 1 → 34/111, current ticket P1-035, fix the health-snapshot line still listing CI-FAIL-012 as open S1 — it is fixed), commit the state update, merge phase-1 into main locally (default `Merge branch ...` subject, never custom lowercase `merge:`). 3) If it fails: read the log first; re-run the failing test standalone, then a fresh full verify; do not touch code on a first flake. Known flakes this run: worker-shutdown "did not exit within 10 s of SIGTERM" (first occurrence, standalone 3/3 — load flake, watch only) and FLAKY-004 readyz 3/4 (passed on retry). 4) Then P1-035 Key rotation (POST `/v1/api-keys/{id}/rotate`, 24 h overlap configurable, audited; AC: overlap window tested).
- No new S1 issues. No push/fetch/rebase done (driver's job). No VM reset during this run.
