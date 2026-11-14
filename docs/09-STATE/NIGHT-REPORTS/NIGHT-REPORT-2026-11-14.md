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

---

## 23rd coordinator (~17:45–20:xx IST) — P1-034 COMMITTED + MERGED; P1-035 implemented, verify pending; 17th VM reset

### P1-034 — DONE, committed `60fa012`, merged to main as `dba9208`
- Adopted verify attempt 5 (bash pid 9812, `.scratch/p134-verify-5.log`, launched 17:30 IST by the 21st). It finished EXIT=101 at ~18:15 IST: **725 passed, 1 failed** — `detag-worker --test shutdown`, `worker_starts_and_exits_zero_on_sigterm`: "worker did not exit within 10 s of SIGTERM" (the ONLY failure; everything else green). P1-034 touches no worker code; isolated re-run **3/3 passes in <1 s** — a load flake under the 32-min parallel run, first occurrence. Recorded as **FLAKY-005** in ISSUES.md (one strike; fix belongs to a hardening ticket, not P1-034).
- **Verify attempt 6** (`.scratch/p134-verify-6.log`, bash pid 15397, launched ~18:25 IST): **GREEN — 728 passed, 0 failed** (718 + 10 new: 2 core unit + 8 extractor), ~33 min; worker-shutdown + FLAKY-002 tests both green. Drift checks green: openapi/route-table/config-table/env/sqlx-prepare/snapshots (compose-prod-check red only on the missing `docker` binary, DOCKER-001 — P1-034 touches no compose files).
- **FLAKY-002 second-leg commit amended** (`0a600f5` → `b9d781c`, message only): the old subject `test(core): fix FLAKY-002 second leg — ...` fails the NEW linter on main (CI-FAIL-019 fix, a5b8425 — id must sit in ticket position + an FR/NFR trace is required; the old subject had neither, and its body said "Full just verify green TBD"). New subject: `test(core): FLAKY-002 second leg — Done dropped when pump still draining burst (NFR-52)`; body updated with the real verify result (728 passed, attempt 6). Verified with main's linter: passes. The commit itself (code) is untouched — still its own commit, not squashed.
- Committed P1-034 as ONE commit `60fa012` (`feat(auth): P1-034 key auth extractor (FR-A-06)`, 26 files, linter passes, gitleaks clean), PROGRESS.md updated (Phase 1 → 34/111, current ticket P1-035, health snapshot + open-issues counts corrected: S1 0 open, S4 now 5 incl. FLAKY-005; next-up queue refreshed), then merged `phase-1` → `main` as `dba9208` (default `Merge branch 'phase-1'` subject, clean, no conflicts). Linter on the merge range `60ecc0f..dba9208`: all 3 commits pass. Local main is now `dba9208` (NOT pushed — driver syncs via deploy key).
- CI context (from the parent, recorded in MEMORY.md, not verified from here): **CI #18 (run #22, on 8a65cd4) is the first FULLY GREEN CI run ever** (verify ✓, smoke ✓, image-size ✓, cargo-deny ✓, gitleaks ✓). **CI-FAIL-019** (linter rejects FLAKY ids) fixed + pushed by the parent (`a5b8425`, merged `60ecc0f`, CHANGELOG regenerated + RENAME.md created as `8d7c716`). CI #19 / Commits #23 will confirm.

### P1-035 — Key rotation: IMPLEMENTED, NOT YET VERIFIED (next coordinator: restore services, drift checks, full verify, commit)
- Design (see phase-file P1-035 status for the full note): `POST /v1/api-keys/{id}/rotate` mints a replacement row (same name + scopes, new secret shown once) and stamps the old row's `expires_at` to `now + PF_API_KEY_ROTATION_OVERLAP_SECS` (default 86400 s, `Settings::api_key_rotation_overlap_secs`, validated >= 1). Old secret valid inside the window, dead after (401 `PF-1001`); rotation exempt from the 10-live-keys cap (replaces, not adds); old row still counts during the overlap, stops counting after. A key cannot rotate (403 `PF-1002` — route not in the extractor's scope table); unknown/foreign/revoked ids are all 404 `PF-2010`. Audited as `api_key.rotated`.
- Migration `20261114131820_api_keys_expires_at` (adds `expires_at TIMESTAMPTZ NULL` + down migration) — **applied to the dev DB with the `sqlx` CLI** (`cargo sqlx migrate run`) BEFORE `just sqlx-prepare`, because the migrate example compiles the lib whose changed queries fail offline until prepared (chicken-and-egg; the CLI applies + records the migration the same way the embedded migrator does — `just migrate` afterwards reported "already up to date"). `.sqlx/` regenerated (3 deleted + 5 new query files).
- Code: `crates/core/src/api_keys.rs` (overlap default const), `crates/core/src/repos/api_keys.rs` (`ApiKey.expires_at`, `by_secret_hash` + `count_live_for_user` take the caller's clock, new `by_id_for_user` + `set_expires_at`), `crates/core/src/config.rs` (new setting + env + validation), `apps/api/src/services/api_keys.rs` (`rotate_api_key` + `RotateApiKeyError`, verify/create call sites pass `state.clock.now()`), `apps/api/src/routes/api_keys.rs` (handler + utoipa), `apps/api/src/routes/auth.rs` (route wired), `apps/api/src/routes/mod.rs` (route table 66 → 67), `apps/api/src/openapi.rs` (path registered — WITHOUT this the regen produced no new path), `.env.example` + runbook + regenerated config table, `just openapi` + `just route-table` regenerated.
- Tests: 7 new in `tests/api_keys.rs` (happy path + audit, frozen-clock overlap window incl. 1 s before/after the deadline, unknown/foreign 404, revoked 404, key-cannot-rotate 403, full-cap rotate succeeds, 9-key rotate + overlap slot-frees) + 1 new repos_live expiry test. Targeted binaries GREEN: api_keys 13/13, extractor 12/12, route_table 3/3, repos_live api_keys 3/3, config lib 35 passed. `cargo fmt --check` clean; `cargo clippy -p detag-api --lib` clean (an earlier clippy "could not compile" line in the drift-check run was the VM reset killing the build mid-flight — re-run clean after).
- Two findings while testing: (1) the first version of the cap test expected a freed slot after rotating at a FULL 10 — wrong: rotation replaces, so 10 live keys stay 10 live keys after rotation; the slot-frees proof needs 9 keys (fixed; the test now uses 9). (2) The route-table count assertion hardcodes 66 — updated to 67 (and the "64 /v1 requests" probe comment to 65). (3) The repos_live expiry test hit the P1-034 microsecond-precision trap — `past` is built with `DateTime::from_timestamp_micros`. (4) New repos_live DB names must be registered in BOTH `drop_db_sql` and `create_db_sql` (`detag_p135_api_keys_expiry` added).
- Docs: api-spec rotate row, data-model `expires_at`, security-testing P1-035 bullet, threat-model T20 row + update-log row, phase-file P1-035 status (box UNticked — tick it only after a green full verify). PROGRESS.md already says current ticket P1-035.
- **Drift checks at ~19:50 IST: openapi-check ✓, config-table-check ✓, env-check ✓, snapshots-check ✓; route-table-check ✗ (stale `docs/api/route-table.md` — FIXED since: `just route-table` re-ran, doc regenerated); sqlx-prepare-check ✗ ONLY because the 17th VM reset had killed postgres (connection refused) — re-run after the restore.**

### Environment: 17th VM reset (~19:12 IST, mid-drift-checks)
- Binaries + data dirs gone entirely (`/usr/bin/psql`, `/usr/bin/redis-cli`, `/etc/redis`, `/var/lib/postgresql/16` all absent). Os-intent replay pid 766 running since 13:42 UTC (`apt-get update` crawling on the cogentco mirror) — WAITING it out per ASSUMPTION-010, no parallel install. Apt cache holds 17 debs. **Restore NOT yet done when this run ended — the next coordinator must: (1) wait for pid 766 to exit, (2) `apt-get update` + `apt-get install -y redis-server postgresql-16` if the replay did not restore the binaries (or `dpkg -i /var/cache/apt/archives/*.deb` if the cache covers it), (3) `pg_ctlcluster 16 main start` + `setsid redis-server /etc/redis/redis.conf --daemonize yes --save "" --maxmemory 256mb --maxmemory-policy allkeys-lru`, (4) recreate the `pf` role/db via `sudo -u postgres psql` (password `pf`, grep-first extraction), (5) `just migrate` (16 migrations — the new `20261114131820` included; the dev DB was fresh-clustered by the reset, so ALL migrations re-apply), (6) re-run the two pending drift checks + full `just verify` on the P1-035 tree.**
- The P1-035 tree survived intact (uncommitted on `main` — NOTE: the branch is `main` right now, NOT `phase-1`, because the P1-034 merge just happened; the next coordinator must get back onto `phase-1` before committing P1-035, per the branch rule: `phase-1` still exists at `60fa012` — `git checkout phase-1 && git merge main` fast-forwards it to `dba9208`, then commit P1-035 there).

### What the next (24th) coordinator must know
- Current ticket: **P1-035 — Key rotation** (implemented, uncommitted — see above). Order: (1) restore services per ASSUMPTION-010 (17th reset — see above); (2) re-run `just route-table-check` + `just sqlx-prepare-check` (both must pass); (3) launch full `just verify` (`.scratch/p135-verify-1.log`, ~33 min) — targeted binaries are already green, so a failure is most likely a flake (FLAKY-001/004/005 family — re-run the failing test standalone first, then a fresh full verify; do NOT touch code on a first flake); (4) if green: tick the P1-035 box + update its phase-file status (TBD → real numbers), update PROGRESS.md (Phase 1 → 35/111, current ticket P1-036), commit ONE commit `feat(auth): P1-035 key rotation (FR-A-06)`, merge `phase-1` → `main` locally (default `Merge branch ...` subject); (5) then P1-036 Key revoke (DELETE immediate; cached key entries invalidated via redis denylist; tested).
- Branch state: local `main` == `dba9208` (P1-034 merge, NOT pushed — driver syncs). P1-035 work is uncommitted ON `main` — move to `phase-1` before committing (`git checkout phase-1 && git merge main` fast-forwards it from `60fa012` to `dba9208`, then commit P1-035 there).
- NEVER push, fetch, set a remote, or rebase — GitHub sync is the driver's job. NEVER force-push.
- `just verify` takes ~33 min; run targeted binaries first when iterating, full verify before committing.

### Update (~20:10 IST, same run): 17th reset RESTORED, P1-035 verify-1 launched
- Replay pid 766 finished ~20:0x IST (binaries back first, then the closing pass). Restored per ASSUMPTION-010: `pg_ctlcluster 16 main start` + setsid redis (PONG), `pf` role/db recreated via `sudo -u postgres psql` (password extraction verified `pw=[pf]`), `just migrate` applied all **16** migrations incl. the new `20261114131820`. ASSUMPTION-010's 17th-reset note in ISSUES.md still says "restore PENDING" — update it when the next run writes its section (the restore IS done).
- Drift checks re-run after the restore: **all 6 PASS** (openapi, route-table, config-table, env, sqlx-prepare, snapshots). `cargo clippy -p detag-api --lib` clean.
- Branch move done: P1-035 tree was uncommitted on `main` after the P1-034 merge; moved to `phase-1` via stash → `checkout phase-1` → `merge main` (fast-forward to `dba9208`) → stash pop. Tree intact (33 entries). On `phase-1` at `dba9208` now.
- **Verify attempt 1 for P1-035 LAUNCHED ~20:12 IST** (`.scratch/p135-verify-1.log`, bash pid 14234) — full `just verify` on the P1-035 tree. Targeted binaries are already green, so expect green in ~33 min (~20:45 IST). If it fails, read the log first; a single FLAKY-001/004/005-family failure means re-run the failing test standalone, then a fresh full verify — do NOT touch code on a first flake.

### Correction (~20:20 IST): verify-1 failed on MY clippy error — fixed, verify-2 launched
- Verify-1 (`.scratch/p135-verify-1.log`, pid 14234) failed in ~2 min at the lint step: `clippy::redundant-locals` denied in `apps/api/src/services/api_keys.rs:268` — my own `let now = now;` inside the create tx closure (added when threading the clock into `count_live_for_user`; the closure already captures the outer `Copy` `now`). Fixed by deleting that line; full-workspace `cargo clippy --all-targets -D warnings` + `cargo fmt --check` clean afterwards. Lesson: `cargo check` does NOT run clippy — always run the clippy command itself before launching a full verify (the earlier clean `cargo clippy -p detag-api --lib` run had been against the pre-reset tree state and missed it).
- **Verify attempt 2 LAUNCHED ~20:24 IST** (`.scratch/p135-verify-2.log`, bash pid 14811) — full `just verify` on the P1-035 tree. Expect green in ~33 min (~20:57 IST).

### 24th coordinator (~19:41–19:44 IST) — CUT OFF BY CRON TIMEOUT while adopting verify-2; NO commits made
- Adopted verify-2 (bash pid 14811, `.scratch/p135-verify-2.log`, launched by the 23rd). At cut-off it is STILL RUNNING and progressing: `cargo test --workspace` (pid 14841) alive, log at the `admin_search` binary (7 `test result: ok` lines so far, no failures, no EXIT line). ps etime 09:16 — clock unreliable, do not use for elapsed-time arithmetic; judge by live `ps` + log growth.
- P1-035 tree intact and uncommitted on branch `phase-1` (33 entries), local main == `dba9208` (P1-034 merge). NOTHING committed, pushed, or changed by this run — the only file touched is this night report.
- CI-FAIL-020 root cause VERIFIED from the repo (not yet filed/committed): `commits.yml` "Install script-test deps" installs only `pillow==10.2.0` + `numpy==1.26.4`, no PyYAML; five modules import yaml (`check_compose_env.py`, `check_compose_health.py`, `check_minio_policy.py`, `validate_prometheus.py` lazy import, `validate_dashboards.py` lazy import) + `test_validate_prometheus.py` imports yaml at line 148 — matches the Commits #23 symptom (3 import ERRORs + validate_prometheus ERRORs/FAILs). Fix: add `pyyaml==6.0.2` to that install step; commit as `fix(ops): CI-FAIL-020 ...` (scope `ops`, NOT `ci`) AFTER P1-035 is committed, per the CI-FIX RULE (root cause first, cross-checked — done above). File it in ISSUES.md first.
- Two corrections still pending (do when updating PROGRESS.md for P1-035): P1-034's real commit is `60fa012`, not `a32cc0e`; ASSUMPTION-010's 17th-reset note still says restore "PENDING" — it is DONE (23rd restored services, 16 migrations applied).
- NEXT COORDINATOR: (1) adopt verify-2 (pid 14811, log `.scratch/p135-verify-2.log`) — do NOT start a second verify; (2) if green: tick P1-035, update PROGRESS.md (35/111, current ticket P1-036, fix the `a32cc0e` hash), commit ONE commit `feat(auth): P1-035 key rotation (FR-A-06)`, merge `phase-1` → `main` locally; (3) then file + fix CI-FAIL-020 as its own commit (see above), then P1-036 Key revoke. If verify-2 fails on a single FLAKY-001/004/005-family test: re-run that test standalone first, then a fresh full verify — no code change on a first flake. NEVER push/fetch/rebase.

---

## 25th coordinator (~19:47 IST–) — P1-035 COMMITTED + MERGED; CI-FAIL-020 FIXED + MERGED; P1-036 implemented, verify running

### P1-035 — DONE, committed `e3c135e`, merged to main (fast-forward)
- Adopted verify-2 (bash pid 14811, `.scratch/p135-verify-2.log`, launched by the 23rd). It finished **GREEN — EXIT=0, 736 passed, 0 failed** (728 + 8 new: 7 api_keys + 1 repos_live), ~38 min; 97 `test result: ok` lines, zero failures. Attempt 1 had failed only on a clippy `redundant-locals` (`let now = now;` in services/api_keys.rs) — one-line fix, no other code change.
- Ticked the P1-035 box + AC in the phase file (status updated with real numbers); updated PROGRESS.md (Phase 1 → 35/111, current ticket P1-036) + the two corrections: P1-034's real commit is `60fa012` (not `a32cc0e`), and ASSUMPTION-010's 17th-reset note corrected in ISSUES.md (restore DONE — 23rd restored services, 16 migrations applied). Committed ONE commit `e3c135e` (`feat(auth): P1-035 key rotation (FR-A-06)`, 31 files, linter passes, gitleaks clean), merged `phase-1` → `main` locally (fast-forward to `e3c135e`, no merge commit — phase-1 was directly ahead; range lint passes). NOT pushed — driver syncs via deploy key.

### CI-FAIL-020 — DONE, committed `128f593`, merged to main (fast-forward)
- Root cause RE-VERIFIED from the repo before committing (CI-FIX RULE): `commits.yml` "Install script-test deps" installs only `pillow==10.2.0` + `numpy==1.26.4`, no PyYAML, while six files import yaml (top-level in `check_compose_env.py:48`, `check_compose_health.py:45`, `check_minio_policy.py:38`; lazy in `validate_prometheus.py:40`, `validate_dashboards.py:172`; `test_validate_prometheus.py:148`). Cross-check: `ci.yml:135` already installs `pyyaml==6.0.2` for the verify job — same pin used. Filed in ISSUES.md first (symptom: Commits #23, 120 tests, 12 problems, all `No module named 'yaml'`).
- Fix: added `"pyyaml==6.0.2"` to the install step + updated the stale CI-FAIL-013 comment (it said only two modules need third-party deps). Verified locally the way the workflow does: `python3 -m unittest discover -s scripts -p "test_*.py"` — **150 tests, OK** (yaml 6.0.1 present locally). Committed ONE commit `128f593` (`fix(ops): CI-FAIL-020 add PyYAML to Commits workflow script-test deps (NFR-52)`, 2 files, linter passes, gitleaks clean), merged `phase-1` → `main` locally (fast-forward to `128f593`). NOT pushed — driver syncs.
- NOTE (from the parent, recorded in MEMORY.md during this run — NOT verified from here): the parent pushed BOTH fixes at ~20:35 IST (user: "dono fix kar") — remote main is now `7efce34`, which also carries the parent's FLAKY-005 fix (`test(worker): FLAKY-005 worker shutdown exit deadline 10s to 30s (NFR-52)`, wait_for_exit only; FLAKY-005 status → fixed) plus P1-035 + CI-FAIL-020. **Correction (post-merge): the divergence described below is RESOLVED — when this run merged `phase-1` → `main`, local main already contained `7efce34` (the parent had updated this clone's main while the coordinator worked), so the merge (`3c42355`) has parents `7efce34` + `25f9b19` and local main is now strictly ahead of remote — a plain fast-forward push works, no force-push ever needed.** CI #21 + Commits #25 will confirm both fixes (they will run on the pushed head, which includes `7efce34` + P1-036). FLAKY-005's ISSUES.md entry was updated by the parent in `7efce34` (status → fixed, second occurrence in CI #20) — it is in this tree via the merge, not touched by this coordinator.

### P1-036 — Key revoke: IMPLEMENTED, verify running (next coordinator: adopt it)
- Design: `DELETE /v1/api-keys/{id}` stamps `revoked_at` in the same tx as the `api_key.revoked` audit (id + name + prefix, no secret/hash), then denylists the key's sha256 in Redis (`pf:revocation:api-key:{hash}`, 24 h TTL — `services::revocation`, mirroring the P1-021 jti denylist). The secret dies immediately (next use 401 `PF-1001`, no grace) and stops counting toward the cap. The extractor checks the denylist BEFORE the DB lookup (fail-open on a Redis error — the DB lookup, which excludes revoked rows, is the authority; P0-103). A key cannot revoke (403 `PF-1002` — route not in the extractor's scope table); unknown/foreign ids are 404 `PF-2010`; re-revoking your own dead key is a no-op 204 with no second audit (DELETE is idempotent — the P1-019 session convention). Repo `revoke` is a conditional UPDATE returning whether a live row was revoked. TTL note: a key never expires, so unlike a jti there is no remaining life to clamp to — 24 h outlives any verification cache entry (there is NO verification cache today; any cache added later must keep its TTL under this one); the DB stays the authority after the entry expires.
- Code: `crates/core/src/repos/api_keys.rs` (`revoke`), `apps/api/src/services/revocation.rs` (`api_key_revocation_key`, `API_KEY_DENYLIST_TTL_SECS = 86_400`, `deny_api_key`, `is_api_key_revoked` + 2 unit tests), `apps/api/src/services/api_keys.rs` (`verify_api_key` denylist check + `revoke_api_key` + errors), `apps/api/src/routes/api_keys.rs` (handler, 204/404/500), `apps/api/src/routes/auth.rs` (replaced `delete(stub)`), `apps/api/src/openapi.rs` (path registered), `.sqlx/` regenerated (1 new query file — no migration: no schema change). One fix while wiring: the revoke service block first landed after `create_api_key` (an edit matched the wrong identical return) — moved to the end of the file after `rotate_api_key` (verify/create/rotate/revoke order restored).
- Tests: 5 new in `tests/api_keys.rs` (happy path incl. denylist + audit, unknown/foreign 404, already-revoked no-op, key-cannot-revoke 403, revoke frees the cap slot) + 1 new repos_live revoke test (`detag_p136_api_keys_revoke`, registered in both drop/create fns). Route-table test updated: DELETE is now a real handler — 401 exception branch, like rotate (its first run failed exactly on this: "must resolve to the 501 stub, got 401" — the test, not the code, needed the update). Targeted binaries GREEN: api_keys 18/18, extractor 12/12, revocation 4/4, repos_live api_keys 4/4, route_table 3/3, openapi 3/3, lib revocation unit 4/4. `cargo fmt --check` + `cargo clippy --workspace --all-targets -- -D warnings` clean. Drift checks green (openapi/route-table/config-table/env/sqlx-prepare/snapshots); `just openapi` regenerated (DELETE path, 204/401/403/404); `just route-table` unchanged (route list already had DELETE).
- Docs: api-spec DELETE row, data-model `revoked_at` + denylist note, security-testing P1-036 bullet, threat-model T20 row + update-log row, phase-file P1-036 status (box UNticked — tick it only after a green full verify).
- **Verify attempt 1 FINISHED GREEN** (`.scratch/p136-verify-1.log`, bash pid 61870): **EXIT=0, 744 passed, 0 failed** (736 + 8 new: 5 api_keys + 1 repos_live + 2 lib revocation unit), ~42 min, attempt 1, no flakes, no retries. 97 `test result: ok` lines. Committed ONE commit `feat(auth): P1-036 key revoke (FR-A-06)`, merged `phase-1` → `main` locally as `3c42355` (parents `7efce34` + `25f9b19` — the merge also pulled the parent's FLAKY-005 fix into local main). NOT pushed — driver syncs with a plain fast-forward push.

### What the next (26th) coordinator must know
- Current ticket: **P1-037 — Per-key rate limits** (tier from config, default lower than user tier; 429 headers; abuse log on repeated trips). P1-036 is DONE (committed, merged — see above).
- **Divergence RESOLVED:** local main == `3c42355` (merge of `7efce34` + `25f9b19`); remote main == `7efce34` per the parent's note — local is strictly ahead, so the driver's next push is a plain fast-forward. Do NOT fetch, push, or force-push — GitHub sync is the driver's job. No FLAKY-005 ISSUES.md work is needed here — its status update came in with `7efce34`.
- Branch state: on `phase-1` (== local main after the P1-036 merge). P1-036 tree committed; tree should be clean.
- NEVER push, fetch, set a remote, or rebase — GitHub sync is the driver's job. NEVER force-push.
- `just verify` takes ~38–42 min; run targeted binaries first when iterating, full verify before committing.
