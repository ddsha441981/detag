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
