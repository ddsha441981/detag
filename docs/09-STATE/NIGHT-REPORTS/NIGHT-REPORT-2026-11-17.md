# NIGHT-REPORT-2026-11-17 — 60th + 61st coordinators

## Tickets done

- **P1-061 — Products/prices sync job (FR-P-01 + FR-P-05): DONE + COMMITTED + MERGED.**
  Implemented by the 60th coordinator, finished by the 61st (adopted the
  uncommitted tree, cross-checked it against the spec + ADR-0029,
  verified independently, committed as one commit `b05f57c`
  (`feat(billing): P1-061 Products/prices sync job (FR-P-05)`), merged
  to `main` as `cf37b8b` (default merge subject, no rebase, no push).
  `lint_commits.py --single b05f57c`: all 1 commit pass. Full
  `just verify` green: **898 passed, 0 failed** (885 + 13 new: 11
  sync-engine `FakeCatalog` tests + 2 catalog_sync endpoint tests),
  attempt 1, ~59 min, no flakes; catalog_sync 2/2, billing lib sync
  tests all ok. Drift checks green (openapi / route-table /
  config-table / env / sqlx-prepare / snapshots) + `cargo deny check`
  green + gitleaks 49 findings, all gitignored build artifacts
  (`.scratch/` 40, `target/` 8, `scripts/__pycache__` 1 — none tracked,
  none in the diff). New `crates/billing/src/sync.rs` (sync engine
  generic over an internal `CatalogProvider`; products matched by
  `detag_plan_code` metadata, ₹0 plans skipped, drift replaces +
  archives the old price, adoption before creation, per-plan failure
  recorded) + `StripeAdapter: CatalogProvider` impl in `stripe.rs`
  (list cap 100, recurring monthly price) + `POST
  /v1/admin/billing/catalog-sync` (admin only, returns the drift
  report, 503 `PF-5005` when unconfigured, audited as
  `admin.billing.catalog_synced`) + `catalog_sync` worker kind as a
  stub + `PF-5005` (`BillingNotConfigured`, 503). Docs: ADR-0029,
  ASSUMPTION-024, pricing-config, phase-file tick + AC, PROGRESS.md
  (Phase 1 61/111, current P1-062), the 43rd- + 44th-reset
  ASSUMPTION-010 lines. Honesty: the Stripe HTTP legs are untested
  live (no test-mode key on this VM); the nightly schedule is P1-082's.

## S1 check (61st, run start)

- 0 open S1. All CI-FAIL-001..020 are fixed (017/018 fixed, waiting on
  CI; 019/020 fixed + pushed). Open items are S2/S3/assumptions/flaky
  hardening candidates — none interrupt. No S1 fix was needed before
  P1-061.

## The 43rd + 44th VM resets (ASSUMPTION-010)

- 43rd reset (~00:51 IST, before the 60th's targeted catalog_sync
  test run): binaries + data dirs gone, apt cache empty, no replay.
  The 60th self-installed in the background (`apt-get update` +
  install, log `p1-061-restore.log`); the restore was still running
  when the 61st started. The 61st waited it out (RESTORE_APT_EXIT=0,
  `CREATE ROLE` / `ALTER ROLE` / `CREATE DATABASE`), then verified
  services itself: postgres accepting, redis PONG, `pf` role
  `rolcreatedb=t`, `pf` db present, `just migrate` applied all 17
  migrations. Full `just verify` then ran from scratch (P1-056 rule).
- 44th reset (~02:53 IST, mid-gitleaks, after the verify + all drift
  checks went green): uptime 0 min at discovery; binaries + data dirs
  gone again, apt cache empty, no replay. The P1-061 tree survived
  intact, uncommitted, on `main` — and the verify had already gone
  green (VERIFY_EXIT=0), so no re-verify was needed. The gitleaks
  JSON report was re-run after the reset (report
  `gitleaks-p1061.json` in hidden_files, same 49 findings, all
  gitignored). Services were re-restoring in the background at commit
  time (log `p1-061-restore2.log`, includes `just migrate` at the
  end) — and the restore COMPLETED during the commit: postgres
  accepting, redis PONG, `pf` CREATEDB, all 17 migrations, verified
  by the 61st before this report was written. No adoption gap for the
  62nd: services are UP.

## Run-end state (61st coordinator)

- Branch `main`, HEAD = cf37b8b (`Merge branch 'phase-1'`).
  `phase-1` HEAD = b05f57c (P1-061). P1-061 DONE, committed, merged,
  NOT pushed (driver syncs — last-pushed.txt still 62e9c20; the driver
  updates it after a verified remote read-back).
- Working tree CLEAN. This night report is a NEW untracked file — it
  rides with the P1-062 commit (the established pattern: the 60th's
  11-16 report edits rode with P1-061).
- Services UP (postgres accepting, redis PONG, `pf` CREATEDB, 17
  migrations). No S1 blockers. No owner decision needed.
- P1-062 NOT started — this run ended after the P1-061 merge; see the
  prep notes below.

## P1-062 prep for the 62nd (read, not yet implemented)

- Spec (phase file): "POST /v1/billing/checkout: auth, plan from
  body, customer reuse, success/cancel URLs from config; idempotency
  honored." AC: idempotent checkout. Deps: P1-061. Trace to FR-P-05
  (Stripe) + FR-P-01 (plan catalog) in the commit.
- This is the FIRST live run of the adapter's HTTP legs
  (ASSUMPTION-023 clause 4, ASSUMPTION-024 clause 6): no Stripe
  test-mode key exists on this VM — do NOT invent one. If no key can
  be obtained, prove checkout against the `FakeProvider` pattern +
  recorded-Stripe-response fixtures and say so honestly in the commit
  body, same as P1-060/P1-061.
- The checkout reads `plans.stripe_price_id` — the P1-061 sync is the
  normal path that fills it; a hand-set id the sync finds drifted is
  replaced (pricing-config doc, path 3).
- Full `just verify` per ticket (user's standing choice); log as
  `p1-062-verify<N>.log` in the same hidden_files dir; launch
  detached, tell the next coordinator to check log tail + live process
  first.

## Next-run plan (first 5)

1. P1-062 — Checkout endpoint (deps P1-061): POST /v1/billing/checkout,
   idempotent checkout; first live run of the adapter's HTTP legs.
2. P1-063 — Webhook receiver (deps P1-062): signature verify, raw-body
   capture, event table insert, dispatch; 4xx only on bad signature.
3. CI #37 results (FLAKY-006 fix confirmation — via the user's logs
   zip); CI #45 on efddaaf was still running as of ~15:41 IST 2026-11-16.
4. CI #45 result when it lands (same source).
5. The 44th-reset services are UP at this report's writing — if a 45th
   reset hits before P1-062 starts, restore per ASSUMPTION-010 and
   re-verify services first (the 62nd does NOT need to re-run the
   P1-061 verify — it went green on this exact tree, log
   `p1-061-verify1.log`, VERIFY_EXIT=0).

## Risks

- The `upgrade_url` `/plans` console page still does not exist until
  Phase 4 (ASSUMPTION-020).
- The adapter's HTTP legs (checkout, portal, subscription fetch,
  invoices, customers, catalog list/create) have never run against
  Stripe (no test-mode key on this VM) — the first live call is
  P1-062's checkout. If the Stripe API surface differs from the 0.41
  docs the code was written against, it fails there, not here.
- A coordinator session being cancelled kills a `setsid nohup` verify
  launched from it — the verify does NOT reliably outlive its
  launching session. If a verify is still running at run end, the
  next coordinator must check the log tail + `ps` first: a log ending
  mid-binary with no live process means re-run from scratch.
- VM resets keep coming roughly every 2 h (43rd ~00:51 IST, 44th
  ~02:53 IST) — each one costs ~25 min of restore (apt-get update
  crawling on the cogentco mirror) plus a from-scratch verify when it
  lands mid-run.

---

# 63rd coordinator — P1-062 finish (in progress at last write)

## Run-start state (Tue 2026-11-17 ~05:34 IST)

- Services verified UP before anything else: postgres accepting,
  redis PONG, `pf` role CREATEDB, DB `pf` has all 17 migrations.
  (The 45th VM reset ~03:33 IST had wiped services before the
  62nd's run; the 62nd restored per ASSUMPTION-010, log
  `p1-062-restore.log`. No restore needed this run — uptime
  ~2h06m at driver check, no new reset since.)
- S1 check: 0 open S1. CI-FAIL-001..020 all fixed (017/018 fixed,
  waiting on CI; 019/020 fixed + pushed). Open items are S2/S3/
  assumptions/flaky hardening candidates — none interrupt. No S1
  fix was needed before P1-062.
- The 62nd was cancelled by the runtime (cron timeout, NOT the
  user) mid-verify, leaving P1-062 IMPLEMENTED but UNCOMMITTED
  on `phase-1`. The 63rd adopted the tree: cross-checked the
  diff (24 files + 4 new `.sqlx/` queries + `checkout.rs` /
  `tests/checkout.rs` / `repos/idempotency.rs`), found it
  complete and correct — one real doc/code mismatch fixed in
  the adoption (see below) — and is finishing it.

## Adoption fix (63rd, before the verify)

- `apps/api/src/services/checkout.rs`: the module doc says "the
  replay check runs before anything else", but the code checked
  `state.billing` (503 `PF-5005` when unconfigured) BEFORE the
  idempotency replay — so a replayed key 503'd while billing was
  unconfigured instead of returning the stored session. Moved
  the provider lookup to just before the provider call (after
  the replay + plan + customer lookups); the replay now truly
  runs first, no provider needed to return a stored session.
  No test changes needed (`checkout_unconfigured_returns_503`
  sends no idempotency key). Targeted suite green after the
  fix: `checkout` 7/7 (~3.5 min).

## The 62nd's verify — NOT adoptable

- Log `p1-062-verify.log` (hidden_files): fmt + clippy green,
  ~40 binaries green, 0 failures — but it ends mid-`lockout`
  with NO `VERIFY_EXIT` (killed by the 62nd's cron timeout).
  Per the P1-056 rule a log ending mid-binary cannot be
  adopted, so the 63rd re-ran `just verify` from scratch as
  attempt 2, log `p1-062-verify2.log` (hidden_files), launched
  detached (`setsid nohup`, pid 49444) at ~00:14 UTC.
- STATUS AT LAST WRITE (update before committing): verify
  attempt 2 running — 31 binaries green, 0 failed, in
  `key_attribution` at ~00:57 UTC (26 green at 00:53 UTC, 21 at
  00:47 UTC — the pace is ~1 binary/min in the slow middle
  section; the full test phase will likely run past this
  session's ~2 h limit). If this run ends before it
  finishes, the next coordinator must check the log tail + `ps`
  first: adopt ONLY if the log shows `VERIFY_EXIT=0` tied to
  this exact tree (every code file's mtime predates the verify
  start); otherwise re-run from scratch.

## P1-062 — DONE + COMMITTED + MERGED (63rd, run end)

- Full `just verify` green: **905 passed, 0 failed**
  (898 + 7 new checkout tests), attempt 2, ~100 min, no
  flakes; 118 binaries, 0 failed. Attempt 1 (62nd) ended
  mid-`lockout` with no `VERIFY_EXIT` (cron timeout) — NOT
  adopted, re-run from scratch per the P1-056 rule. Drift
  checks green (openapi / route-table / config-table / env /
  sqlx-prepare / snapshots) + `cargo deny check` green +
  gitleaks 49 findings, all gitignored build artifacts
  (none tracked, none in the diff). Targeted `checkout` 7/7
  green after the adoption fix (replay-before-provider
  reorder). Committed as one commit `feat(billing): P1-062
  Checkout endpoint (FR-P-03)` on `phase-1` (hash in `git
  log`), merged `phase-1` → `main` locally (default merge
  subject, no rebase, NO push; driver syncs).
- MTIME NOTE: at 01:33:13 UTC a bulk mtime refresh touched
  ~101 `.rs` files workspace-wide within ~2 ms (including
  files never edited in this ticket) — content did NOT change
  (`git diff --stat` byte-identical to the pre-verify reading,
  24 files 299+/57-, re-diffed files match the pre-verify
  review line for line); the verify's 905 green tests ran on
  this exact content, so the commit stands and CI re-verifies
  on push. Documented in the phase-file status line and in
  PROGRESS.md (health snapshot + P1-062 note).
- Run-end state: branch `main`, HEAD = the P1-062 merge.
  `phase-1` HEAD = the P1-062 commit. P1-062 DONE, committed,
  merged, NOT pushed (driver syncs — last-pushed.txt still
  cf37b8b; the driver updates it after a verified remote
  read-back). Working tree CLEAN. Services UP (postgres
  accepting, redis PONG, `pf` CREATEDB, 17 migrations). No S1
  blockers. No owner decision needed. No VM reset during this
  run (45th was before it, ~03:33 IST).
- Next: P1-063 — Webhook receiver (`POST /v1/webhooks/stripe`:
  constant-time signature verify, raw-body capture, event
  table insert with unique event id for idempotency, then
  dispatch; 4xx only on bad signature). AC: tamper rejected,
  replay no-op. Deps: P1-062. No Stripe test-mode key exists
  on this VM — do NOT invent one; prove against recorded
  fixtures / the fake provider and say so honestly, same as
  P1-060/P1-061/P1-062.

## P1-062 — what the commit will contain (when verify goes green)

- `POST /v1/billing/checkout` (JWT only; API key 403
  `PF-1002`, anonymous 401 `PF-1001`): body `{"plan_code"}` →
  `{"id", "url"}`; provider in `AppState` as
  `Option<DynBillingProvider>` (`None` → 503 `PF-5005`);
  customer reuse = newest `provider_customer_id` on any
  subscription row (live or canceled); success
  `{PF_PUBLIC_BASE_URL}/billing/success?session_id={CHECKOUT_SESSION_ID}`,
  cancel `{PF_PUBLIC_BASE_URL}/billing/cancel`; unknown plan
  404 `PF-2010`, Free / no-price 422 `PF-2002`
  (`not_purchasable`), configured-price-unknown-at-Stripe 500
  `PF-5000`, other provider failures 500 with no message
  echoed. Idempotency is the `idempotency_keys` table
  (envelope `{"request", "response"}`, exact-JSON compare,
  24 h TTL): same key + same request replays with no provider
  call, same key + different request 409 `PF-2011`, expired
  row is a fresh checkout. No audit row; no duplicate-purchase
  guard (P1-069's). 7 new tests in `apps/api/tests/checkout.rs`
  (+ 4 new `.sqlx/` queries). Docs: ADR-0030,
  ASSUMPTION-025, api-spec row, test-strategy section,
  security-testing bullet + matrix counts (13 Allowed /
  29 Forbidden / 14 Unprotected / 9 Ignored). Honesty: the
  Stripe HTTP legs are STILL untested live (no test-mode key
  on this VM) — the suite proves the endpoint against a
  `FakeProvider`; the P1-060 note's "first live run is P1-062's
  integration test" is corrected (first live run is the
  endpoint with a real test-mode key, whenever one exists).
- Commit subject (when green): `feat(billing): P1-062
  Checkout endpoint (FR-P-03)` — then merge `phase-1` →
  `main` locally (default merge subject, no rebase, NO push),
  tick P1-062 in the phase file (already ticked by the 62nd —
  fill in the real verify count in its status line), update
  PROGRESS.md (Phase 1 62/111, current P1-063), and update
  this report. Phase file's "[verify count TBD]" MUST be
  replaced with the attempt-2 count before committing.
- Next ticket: P1-063 — Webhook receiver (deps P1-062):
  signature verify, raw-body capture, event table insert,
  dispatch; 4xx only on bad signature.
