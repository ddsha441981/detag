# NIGHT-REPORT-2026-11-15 — 31st coordinator (run ended by cron timeout, NOT by completion)

## What was done

- **P1-042 DONE + merged to main (locally):** adopted the 30th
  coordinator's uncommitted tree (new `apps/api/src/authz.rs` +
  `apps/api/tests/authz_matrix.rs`, matrix-driven guards in
  `apps/api/src/auth.rs`, api-spec / test-strategy /
  security-testing docs), verified it (targeted: authz unit 4/4,
  authz_matrix 3/3; full `just verify` green **776 passed, 0
  failed** — independently confirming the 30th's reported 776),
  committed as one commit `9ae9c5b feat(auth): P1-042 Roles +
  permission matrix (FR-A-07)`, lint-clean (`just lint-commits`
  ok), merged into `main` as `b9144a7`. Phase 1 now **42/111**.
  One fix while adopting: the authz module doc said "20 admin
  routes" but `ROUTE_TABLE` holds 19 — corrected (the public
  `POST /v1/abuse` intake is not an admin route) + `cargo fmt`.
  Drift checks green (openapi / route-table / config-table /
  env / sqlx-prepare / snapshots).
- **22nd VM reset (~07:42 IST, mid-authz_matrix test):**
  restored per ASSUMPTION-010 — no replay running, binaries
  gone, `just` + cargo survived; `sudo apt-get update` (~10
  min, cogentco mirror, one connection failure on
  noble-security InRelease retried) + install redis-server +
  postgresql-16; `pg_ctlcluster 16 main start` + setsid redis
  (PONG); `pf` role/db recreated (password `pf`); `just
  migrate` applied all 17 migrations. Update line added to
  ASSUMPTION-010 in the P1-042 commit. No other reset this run.
- **P1-043 IMPLEMENTED (not committed):** the guard is on all
  19 admin routes — `admin_stub` / `admin_read_stub` in
  `apps/api/src/routes/admin.rs` (matrix guard first, 501
  after); new `apps/api/tests/admin_guard.rs` (3 tests: the
  19-route walk, the audit-row proof, denied/stub audit
  nothing); route-table probe expects 401 on every admin
  route; API-key matrix flips the 14 admin-stub rows
  `Unprotected` → `Forbidden` (counts 13/28/15/9); three doc
  comments fixed for the baked-in `<redacted>` pipeline trap
  (`routes/admin.rs`, `auth.rs`, `api_key_rate_limits.rs` —
  the word after "Bearer" is redacted in transit, so never
  write that phrase in a comment again). Docs: api-spec guard
  note, test-strategy P1-043 section, security-testing P1-043
  bullet. Targeted green: admin_guard 3/3, api_key_matrix
  4/4, authz_matrix 3/3, route_table 3/3. The phase-file
  checkbox + PROGRESS.md ARE updated for P1-043 (43/111,
  current ticket P1-044) but the phase-file Status line says
  the full verify is PENDING — fill in the real numbers
  after the verify below greens. Full `just verify` is
  RUNNING — see below.

## Verify results

| Run | Result |
| --- | --- |
| P1-042 targeted (`detag-api --lib authz`) | 4/4 passed |
| P1-042 targeted (`authz_matrix`, attempt 1) | 2/3 — `admin_matrix_live` failed: Postgres connection-refused (22nd VM reset mid-run, NOT a code failure) |
| P1-042 targeted (`authz_matrix`, after restore) | 3/3 passed, 44.18 s |
| P1-042 full verify, attempt 1 | died at fmt-check (over-long assert line from the "20 routes" doc fix — fixed with `cargo fmt`) |
| P1-042 full verify, attempt 2 | **green: 776 passed, 0 failed**, ~32 min, no flakes |
| P1-042 drift checks | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all green |

| P1-043 targeted (admin_guard) | 3/3 passed, 79.52 s — first run 1/3: the two audit tests failed because registration/login audit against the same user target (3 rows, not 1) — fixed by filtering by action (`audit_for_user_action`), re-run 3/3 |
| P1-043 targeted (route_table + api_key_matrix + admin_guard + authz_matrix) | all green: route_table 3/3, api_key_matrix 4/4 (14 flipped rows), admin_guard 3/3, authz_matrix 3/3 |
| P1-043 full verify | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/p1-042-unit.log`, `.scratch/p1-042-matrix.log`,
`.scratch/verify-p1-042.log`, `.scratch/restore-22.log`,
`.scratch/restore-22-migrate.log`, `.scratch/drift-p1-042-*.log`,
`.scratch/p1-043-guard.log`, `.scratch/p1-043-targeted.log`,
`.scratch/verify-p1-043.log`.

## Resume state for the 32nd coordinator

- **Branch:** `phase-1` (checked out). **Main** is at `b9144a7`
  (P1-042 merge). Nothing is pending push (sync is the
  driver's job).
- **Current ticket:** P1-043 — Admin route guard. Phase 1: 43/111
  (phase file + PROGRESS.md already updated).
- **P1-043 full verify RUNNING:** `just verify`, log
  `~/workspace/detag/.scratch/verify-p1-043.log`. Adopt it by
  log — do NOT restart it. Expected result: **779 passed, 0
  failed** (776 + 3 new admin_guard tests). If it goes green:
  (1) run the drift checks (`just openapi-check`,
  `just route-table-check`, `just config-table-check`,
  `just env-check`, `just sqlx-prepare-check`,
  `just snapshots-check` — all must be green; P1-043 touches
  no routes/config/migrations, but CI parity requires the
  check); (2) fill the real verify numbers into the P1-043
  Status line in `docs/07-PHASES/PHASE-1-identity-billing.md`
  (it currently says PENDING) and the PROGRESS.md
  health-snapshot verify line + P1-043 note (both currently
  say the verify is running); (3) commit as one commit —
  suggested subject `feat(auth): P1-043 Admin route guard
  (FR-A-07)` (linter will confirm); (4) merge `phase-1` into
  `main` locally (plain merge, default subject, no rebase,
  no squash, NEVER push).
- **Then:** P1-044 — Support read-only (Type: feat, Area:
  auth/rbac, Est: M, Deps: P1-043; spec: Support can read
  user/job/admin views only; writes 403; matrix asserts).
- **Environment:** services are UP (postgres :5432 accepting,
  redis PONG, `just` + cargo on PATH via `~/.local/bin` +
  `~/.cargo/bin`). If a reset hits, restore per
  ASSUMPTION-010 and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed. CI #27 + Commits #27 both SUCCESS
  (third consecutive fully-green run); CI #28 + Commits #28
  (on 3348ce3) were in progress at run start — results come
  via the user's logs zip, the coordinator cannot check GitHub
  (2FA SMS does not reach the user, Gmail not connected).
- **Working tree (uncommitted, on `phase-1`):** P1-043 — `M
  apps/api/src/routes/admin.rs`, `M apps/api/src/auth.rs`,
  `M apps/api/tests/api_key_matrix.rs`, `M
  apps/api/tests/api_key_rate_limits.rs`, `M
  apps/api/tests/route_table.rs`, `?? 
  apps/api/tests/admin_guard.rs`, `M
  docs/03-ARCHITECTURE/03-api-spec.md`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/05-QUALITY/03-security-testing.md`, `M
  docs/07-PHASES/PHASE-1-identity-billing.md`, `M
  docs/09-STATE/PROGRESS.md`, `??` this night-report file.
  Do not discard any of them.

---

# NIGHT-REPORT-2026-11-15 — 32nd coordinator (appended; run in progress at write time)

## What was done

- **P1-043 DONE + merged to main (locally):** adopted the
  31st coordinator's running `just verify` by log (pid
  27032, log `.scratch/verify-p1-043.log`) — do NOT restart
  was honoured. It finished green **779 passed, 0 failed**
  (776 + 3 new admin_guard), attempt 1, ~48 min, no flakes
  ("verify OK: fmt + clippy + tests green"). Drift checks
  re-run with real exit codes, all 6 green (openapi /
  route-table / config-table / env / sqlx-prepare /
  snapshots — logs `.scratch/drift-p1-043-*.log`). Filled
  the real numbers into the P1-043 Status line in the phase
  file + the PROGRESS.md health snapshot + P1-043 note.
  Committed as one commit `bf5e6a4 feat(auth): P1-043 Admin
  route guard (FR-A-07)` (12 files; the night-report file
  stays uncommitted, per convention), `just lint-commits`
  ok, merged `phase-1` into `main` as `c7014ed`. Phase 1 now
  **43/111** at that point.
- **23rd VM reset (~09:41 IST, just after the P1-043
  verify finished):** the runtime restarted while the
  32nd coordinator waited on the verify — the verify log
  survived, so no re-run was needed. The os-intent replay
  (pid 766) was running; waited ~3.5 min, did NOT install
  in parallel; the replay DID restore the postgres/redis
  binaries itself. Restored per ASSUMPTION-010
  (`pg_ctlcluster 16 main start` + setsid redis, PONG;
  `pf` role/db recreated via `sudo -u postgres psql`,
  password `pf`; `just migrate`, all 17 migrations
  applied cleanly). Update line added to ASSUMPTION-010
  in the P1-043 commit. `just` + cargo survived.
  NOTE: one exec result during this run carried a system
  safety notice flagging the `ps aux` output (it dumps the
  os-intent replay script as a process command line) —
  nothing from that output was executed or followed; the
  restore followed only the task-prescribed ASSUMPTION-010
  steps.
- **P1-044 DONE + merged to main (locally):** Support
  read-only — new `apps/api/tests/support_readonly.rs` (4
  tests, see the paragraph above for what they prove).
  Targeted green: support_readonly 4/4 (56.80 s, first
  run). Docs: api-spec support note, test-strategy P1-044
  section, security-testing P1-044 bullet. Full `just
  verify` green **783 passed, 0 failed** (779 + 4 new),
  attempt 1, ~46 min, no flakes (log
  `.scratch/verify-p1-044.log`); drift checks green
  (openapi / route-table / config-table / env /
  sqlx-prepare / snapshots — logs
  `.scratch/drift-p1-044-*.log`). Committed as one commit
  `8b654cb feat(auth): P1-044 Support read-only (FR-A-07)`
  (6 files; the night-report file stays uncommitted, per
  convention), `just lint-commits` ok, merged `phase-1`
  into `main` as `9e8dbd0`. Phase 1 now **44/111** at that
  point.
- **P1-045 IMPLEMENTED (not committed):** IDOR sweep v1 —
  new `apps/api/tests/idor_sweep.rs` (3 tests): live keys
  (B revoking / rotating A's key is 404 `PF-2010`, the key
  still authenticates afterwards, A revoking it is 204,
  then it is 401) and live sessions (B's list never
  contains A's session, B deleting A's session is 404,
  A's session still listed afterwards, A deleting it is
  204); the 18 stub resource routes (jobs, uploads,
  webhook-endpoints, api-keys list) answer 501 `PF-5004`
  to an authenticated cross-user caller — no data, no 200,
  no 403 — with the 404 ownership proof landing with each
  handler ticket. Targeted green: idor_sweep 3/3 (35.81 s,
  first run). Docs: test-strategy P1-045 section,
  security-testing P1-045 bullet. Phase file ticked +
  PROGRESS.md pre-updated (45/111, current ticket P1-046)
  with the verify PENDING — fill in the real numbers
  after the verify below greens. Full `just verify` is
  RUNNING — see below. NOTE: verify attempt 1 died at
  fmt-check within seconds (two over-long `probe` lines
  in the new test file — `cargo test` does not run fmt, so
  the targeted run did not catch it); fixed with `cargo
  fmt` (only `idor_sweep.rs` changed), attempt 2 running.

## Verify results

| Run | Result |
| --- | --- |
| P1-043 full verify (31st's run, adopted by log) | **green: 779 passed, 0 failed**, attempt 1, ~48 min, no flakes |
| P1-043 drift checks (re-run, real exit codes) | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-043 lint-commits | ok: `bf5e6a4 feat(auth): P1-043 Admin route guard (FR-A-07)` |
| P1-044 targeted (`support_readonly`) | **4/4 passed**, 56.80 s, first run |
| P1-044 full verify | **green: 783 passed, 0 failed**, attempt 1, ~46 min, no flakes |
| P1-044 drift checks | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-044 lint-commits | ok: `8b654cb feat(auth): P1-044 Support read-only (FR-A-07)` |
| P1-045 targeted (`idor_sweep`) | **3/3 passed**, 35.81 s, first run |
| P1-045 full verify, attempt 1 | died at fmt-check (two over-long lines in the new test file — fixed with `cargo fmt`) |
| P1-045 full verify, attempt 2 | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-043.log`,
`.scratch/drift-p1-043.log`, `.scratch/drift-p1-043-*.log`,
`.scratch/restore-23.log`, `.scratch/restore-23-migrate.log`,
`.scratch/p1-044-targeted.log`, `.scratch/verify-p1-044.log`,
`.scratch/drift-p1-044-*.log`, `.scratch/p1-045-targeted.log`,
`.scratch/verify-p1-045.log`.

## Resume state for the 33rd coordinator

- **Branch:** `phase-1` (checked out). **Main** is at
  `9e8dbd0` (P1-044 merge). Nothing is pending push (sync
  is the driver's job).
- **Current ticket:** P1-045 — IDOR sweep v1. Phase 1:
  45/111 (phase file + PROGRESS.md already updated).
- **P1-045 full verify RUNNING:** `just verify`, pid
  42537 (detached via setsid nohup, started ~10:44 IST —
  attempt 2; attempt 1 died at fmt-check, fixed with
  `cargo fmt`),
  log `~/workspace/detag/.scratch/verify-p1-045.log`.
  Adopt it by log — do NOT restart it. Expected result:
  **786 passed, 0 failed** (783 + 3 new idor_sweep
  tests). If it goes green: (1) run the drift checks
  (`just openapi-check`, `just route-table-check`, `just
  config-table-check`, `just env-check`, `just
  sqlx-prepare-check`, `just snapshots-check` — all must be
  green; P1-045 touches no routes/config/migrations, but
  CI parity requires the check); (2) fill the real verify
  numbers into the P1-045 Status line in
  `docs/07-PHASES/PHASE-1-identity-billing.md` (it
  currently says PENDING) and the PROGRESS.md P1-045 note
  (currently says the verify is running) — the health
  snapshot verify line still says 783, update it to the
  P1-045 count; (3) commit as one commit — suggested
  subject `test(auth): P1-045 IDOR sweep v1 (FR-A-07)`
  (linter will confirm; run `just lint-commits`); (4)
  merge `phase-1` into `main` locally (plain merge,
  default subject, no rebase, no squash, NEVER push).
- **Then:** P1-046 — Privileged action audit middleware
  (Type: feat, Area: auth/rbac, Est: S, Deps: P1-045;
  spec: Route metadata drives automatic audit for admin
  routes (no forgotten manual calls); rows asserted).
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). If a reset hits,
  restore per ASSUMPTION-010 and add an update line.
  Disk: 99G free, but `target/` is 125G — DISK-QUOTA-001
  is still the standing S2; `cargo clean` is the
  mitigation if a build ever SIGBUSes.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed.
- **Working tree (uncommitted, on `phase-1`):** P1-045 —
  `?? apps/api/tests/idor_sweep.rs`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/05-QUALITY/03-security-testing.md`, `M
  docs/07-PHASES/PHASE-1-identity-billing.md`, `M
  docs/09-STATE/PROGRESS.md`, `??` this night-report file.
  Do not discard any of them.

---

# NIGHT-REPORT-2026-11-15 — 35th coordinator (appended; run in progress at write time)

## What was done

- **P1-046 DONE + pushed (driver):** the 34th coordinator's
  commit `d834df9 feat(auth): P1-046 Privileged action audit
  middleware (FR-A-07)`, merged as `251a987`. The driver
  pushed it; local and remote were in sync at `251a987`
  at this run's start. Phase 1 was **46/111**.
- **P1-047 DONE + merged to main (locally):** adopted the
  34th coordinator's uncommitted tree (new
  `docs/06-DELIVERY/07-admin-guide.md` + doc fixes in
  `authz.rs`, api-spec, test-strategy) and its running
  `just verify` (pid 46037, log
  `.scratch/verify-p1-047.log`) — the run was KILLED by
  the 25th VM reset mid-admin_impersonate_audit, so it was
  NOT adoptable; re-run from scratch (attempt 2, log
  `.scratch/verify-p1-047-attempt2.log`). One fix while
  adopting: the `authz_matrix.rs` module doc still said
  the 14 stub rows "take no extractor yet" — stale since
  P1-043; corrected to "enforced promises". Cross-checked
  the guide's matrix + audit actions against
  `Capability::for_route` + `audit_action_for` (all 9
  actions match). Full `just verify` green **793 passed, 0
  failed** (same count as P1-046 — docs-only), attempt 2,
  ~41 min, no flakes. Drift checks green (openapi /
  route-table / config-table / env / sqlx-prepare /
  snapshots — real exit codes). Committed as one commit
  `6ecf10e docs(docs): P1-047 Authz docs — admin guide +
  matrix change procedure (FR-A-07)`, `just lint-commits`
  ok, merged `phase-1` into `main` as `7dbae45`; state
  commit `0dfddba docs(docs): P1-047 state — tick +
  PROGRESS (FR-A-07)`, merged as `8e05e92`. Phase 1 now
  **47/111**.
- **25th VM reset (~13:47 IST, mid-P1-047 verify attempt
  1):** the runtime restarted while the 35th coordinator
  waited on the adopted verify — the verify log survived
  (ends mid-admin_impersonate_audit, 7 tests), so the kill
  is proven, not guessed. No os-intent replay was running;
  binaries gone entirely (no `/var/lib/postgresql`, no
  `/etc/redis`, apt cache empty). Self-installed: `sudo
  apt-get update` (71.1 MB, 11.5 min on the cogentco
  mirror) + `sudo apt-get install -y redis-server
  postgresql-16` (~12 min total); `pg_ctlcluster 16 main
  start` + setsid redis (PONG); `pf` role/db recreated
  via `sudo -u postgres psql`, password `pf`; `just
  migrate` applied all 17 migrations. `just` + cargo
  survived. Update line for ASSUMPTION-010 NOT yet added
  to ISSUES.md — the next coordinator should add it in
  the P1-048 commit (the 25th reset, ~13:47 IST, same
  restore steps as the 22nd).
- **P1-048 IMPLEMENTED (not committed):** Role escalation
  tests — new `apps/api/tests/role_escalation.rs` (4
  tests): plain user × all 19 admin routes 403, support ×
  all 9 writes 403, an admin user's own full-scope key ×
  all 19 routes 403, each denied write asserting no audit
  row for its action with the caller as actor (actor-scoped,
  not absolute count — the harness DB persists across
  runs), plus live-write target state proofs (denied
  suspend leaves target `active`, denied restore leaves a
  suspended target `suspended`, denied impersonate mints
  no row); and a forged-role test — a plain user's token
  with `role` rewritten to `admin` (hand-rolled base64url
  pair, original signature kept) is 401 `PF-1010` on a
  read and on a write, same for a role forged to
  `support`. The spec's "all 403 + audited" is read under
  the P1-043/P1-046 rule ("the trail records actions, not
  attempts"): the audit behaviour asserted is that a
  denial writes nothing — documented in the test module
  doc, the test-strategy P1-048 section, and the
  security-testing P1-048 bullet. Targeted green:
  role_escalation 4/4 (twice, ~75 s each run). Two fixes
  while implementing: dead-code warning on the unused
  `target_type`/`has_id` struct fields (removed) and a
  clippy `manual_is_multiple_of` lint in the base64url
  decoder (fixed, fmt + clippy clean). Docs: test-strategy
  P1-048 section, security-testing P1-048 bullet. Full
  `just verify` is RUNNING — see below.

## Verify results

| Run | Result |
| --- | --- |
| P1-047 full verify, attempt 1 (34th's run, adopted by log) | KILLED by the 25th VM reset mid-admin_impersonate_audit — log ends mid-binary, no result |
| P1-047 full verify, attempt 2 | **green: 793 passed, 0 failed**, ~41 min, no flakes |
| P1-047 drift checks | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-047 lint-commits | ok: `6ecf10e docs(docs): P1-047 Authz docs — admin guide + matrix change procedure (FR-A-07)` |
| P1-048 targeted (`role_escalation`, run 1) | **4/4 passed**, 74.59 s — with 1 clippy dead-code warning (unused struct fields) |
| P1-048 targeted (`role_escalation`, run 2) | **4/4 passed**, 74.04 s — after the dead-code fix; fmt + clippy clean |
| P1-048 full verify | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-047.log` (killed attempt 1),
`.scratch/verify-p1-047-attempt2.log` (green),
`.scratch/verify-p1-048.log` (running).

## Resume state for the 36th coordinator

- **Branch:** `phase-1` (checked out). **Main** is at
  `8e05e92` (P1-047 state merge). Nothing is pending push
  (sync is the driver's job — NEVER push).
- **Current ticket:** P1-048 — Role escalation tests.
  Phase 1: 47/111 (phase file + PROGRESS.md NOT yet
  updated for P1-048 — do that after the verify greens).
- **P1-048 full verify RUNNING:** `just verify`, pid
  18328 (started ~14:48 IST via nohup, log
  `~/workspace/detag/.scratch/verify-p1-048.log`).
  Adopt it by log — do NOT restart it. Expected result:
  **797 passed, 0 failed** (793 + 4 new role_escalation
  tests). If it goes green: (1) run the drift checks
  (`just openapi-check`, `just route-table-check`, `just
  config-table-check`, `just env-check`, `just
  sqlx-prepare-check`, `just snapshots-check` — all must
  be green; P1-048 touches no routes/config/migrations,
  but CI parity requires the check); (2) add the 25th
  VM-reset update line to ASSUMPTION-010 in
  `docs/09-STATE/ISSUES.md` (see above — the restore
  details); (3) tick the P1-048 checkbox + Status line in
  `docs/07-PHASES/PHASE-1-identity-billing.md` and update
  `docs/09-STATE/PROGRESS.md` (48/111, current ticket
  P1-049, health-snapshot verify line, P1-048 note, next-up
  queue); (4) commit as one commit — suggested subject
  `test(auth): P1-048 Role escalation tests (FR-A-07)`
  (linter will confirm; run `just lint-commits`); (5)
  merge `phase-1` into `main` locally (plain merge,
  default subject, no rebase, no squash, NEVER push).
- **Then:** P1-049 — Plans service (Type: feat, Area:
  billing/plans, Est: M, Deps: —; spec: Catalog from db
  (seeded) + cache; entitlement struct {daily_images,
  daily_video_seconds, max_bytes, result_ttl_hours,
  concurrency, lane, features}; effective-plan resolution
  (subscription override)).
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). If a reset hits,
  restore per ASSUMPTION-010 and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed. CI #27 + Commits #27 both SUCCESS
  (third consecutive fully-green run); CI #28 + Commits #28
  (on 3348ce3) were in progress at run start — results come
  via the user's logs zip, the coordinator cannot check GitHub
  (2FA SMS does not reach the user, Gmail not connected).
- **Working tree (uncommitted, on `phase-1`):** P1-048 —
  `?? apps/api/tests/role_escalation.rs`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/05-QUALITY/03-security-testing.md`, `??` this
  night-report file. Do not discard any of them.

---

# NIGHT-REPORT-2026-11-15 — 35th coordinator, closing (run ended by cron timeout, NOT by completion)

## What was done (final)

- **P1-048 DONE + merged to main (locally):** full `just
  verify` green **797 passed, 0 failed** (793 + 4 new
  role_escalation tests), attempt 1, ~48 min, no flakes
  (log `.scratch/verify-p1-048.log`; role_escalation 4/4
  inside the full run). Drift checks green (openapi /
  route-table / config-table / env / sqlx-prepare /
  snapshots — all exit 0, logs `/tmp/drift-p1-048-*.log`).
  Committed as one commit `f813464 test(auth): P1-048 Role
  escalation tests (FR-A-07)` (6 files: the test file,
  test-strategy + security-testing docs, phase-file tick +
  Status line, PROGRESS.md, and the ASSUMPTION-010 25th
  reset update line), `just lint-commits` ok, merged
  `phase-1` into `main` as `4356bec`. Phase 1 now
  **48/111**. Nothing is pending push (sync is the
  driver's job — NEVER push).
- **P1-047 DONE** (see the section above): `6ecf10e`,
  merged as `7dbae45`; state `0dfddba`, merged as
  `8e05e92`. Verify green 793 passed.

## Verify results (final)

| Run | Result |
| --- | --- |
| P1-048 full verify | **green: 797 passed, 0 failed**, attempt 1, ~48 min, no flakes |
| P1-048 drift checks | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-048 lint-commits | ok: `f813464 test(auth): P1-048 Role escalation tests (FR-A-07)` |

## Resume state for the 36th coordinator

- **Branch:** `main` (checked out). **Main** is at
  `4356bec` (P1-048 merge). `phase-1` is at `f813464`
  (same content, merged). Nothing is pending push.
- **Current ticket:** P1-049 — Plans service. Phase 1:
  48/111 (phase file + PROGRESS.md already updated).
  Nothing is in flight: no verify running, no uncommitted
  ticket work — only this night-report file is untracked.
- **P1-049 spec (from the phase file):** Type: feat,
  Area: billing/plans, Est: M, Deps: —. Catalog from db
  (seeded) + cache; entitlement struct {daily_images,
  daily_video_seconds, max_bytes, result_ttl_hours,
  concurrency, lane, features}; effective-plan resolution
  (subscription override). AC: resolution tested.
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). If a reset hits,
  restore per ASSUMPTION-010 (25th reset now recorded
  there) and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed.
- **Working tree (uncommitted, on `main`):** only `??`
  this night-report file. Do not discard it.

---

# NIGHT-REPORT-2026-11-15 — 37th coordinator (run ended by cron timeout, NOT by completion)

## What was done (final)

- **P1-049 DONE + merged to main (locally):** new
  `crates/core/src/plans.rs` — `Entitlements` (all 7 keys,
  fail-closed parse) + `PlanCatalog` over `PlanStore` /
  `PlanCache` traits (the `flags.rs` shape:
  `SqlxPlanStore` + `InMemoryPlanCache`, 5-minute
  `PLAN_CACHE_TTL`, explicit `invalidate`; Redis impl
  later per NFR-10) + `effective_plan` in
  `apps/api/src/services/entitlements.rs` (newest live
  `trialing`/`active` subscription overrides the Free
  default; `past_due`/`canceled` grant nothing; newest
  live wins, not dearest; missing Free plan errors
  `MissingFreePlan`). The catalog is wired into `AppState`
  (one shared instance, built in `from_settings`) and the
  profile now resolves through `effective_plan`. New
  `apps/api/tests/plans.rs` (7 tests). One fix while
  implementing: clippy `derivable_impls` on the manual
  `Default` for `Lane` (now derived with `#[default]`).
  Doc fix in the same commit: the stale
  `subscriptions::active_for_user` comment claiming a
  `past_due` subscription "still grants the plan" (the SQL
  only selects `trialing`/`active`; ADR-0025 pins it).
  Docs: test-strategy P1-049 section, data-model plans row
  pointer, ADR-0025. Committed as one commit `0b29d77
  feat(billing): P1-049 Plans service (FR-P-01)` (13 files),
  `just lint-commits` ok, merged `phase-1` into `main` as
  `42e5677`. Phase 1 now **49/111**. Nothing is pending
  push (sync is the driver's job — NEVER push).
- **26th VM reset (~15:42 IST, mid-targeted-tests):**
  restored per ASSUMPTION-010 — replay pid 765 ran ~3.5
  min and restored the postgres/redis binaries itself;
  `pg_ctlcluster 16 main start` + setsid redis (PONG);
  `pf` role/db recreated (password `pf`); `just migrate`
  applied all 17 migrations. Update line added to
  ASSUMPTION-010 in the P1-049 commit. One near-miss while
  writing that update: the first edit landed the line under
  TESTKIT-MINIO-001 (the edit tool matched the first
  `- Status: open` in the file) — caught by grep, removed,
  and re-inserted at the end of ASSUMPTION-010 (line 216);
  verified by re-reading the section.
- **P1-050 IMPLEMENTED, NOT COMMITTED (verify running
  for the 38th coordinator to adopt):** new
  `crates/core/src/usage.rs` — `UsageMeter` (Redis
  day-hash via one atomic Lua script + DB write-through
  via `usage_counters::increment`; `reconcile_day` with
  the DB winning, compare-and-set) + `estimate_video_seconds`
  (known duration rounded up; unknown reserves nothing,
  ASSUMPTION-016) + `crates/core/src/usage_record.lua` /
  `usage_reconcile.lua` + `repos::usage_counters::increment`
  / `overwrite` + new `crates/core/tests/usage_live.rs`
  (4 live tests, incl. the AC: UTC-boundary
  clock-injected test) + `.sqlx/` regen (2 new query
  files). Targeted green: usage unit 3/3, usage_live
  4/4 (boundary, reconcile drift repair, 10 concurrent
  records = exactly 10 in both stores, dead-Redis fails
  closed with no DB row), fmt + clippy `-D warnings`
  clean on detag-core. Docs drafted in the tree:
  test-strategy P1-050 section, data-model
  usage_counters pointer, ASSUMPTION-016. NOT done:
  full `just verify` (RUNNING — see below), drift
  checks, phase-file tick + PROGRESS.md, commit, merge.
  That is the 38th coordinator's job — adopt the verify
  by log, do NOT restart it.

## Verify results (final)

| Run | Result |
| --- | --- |
| P1-049 targeted (core plans unit) | 5/5 passed |
| P1-049 targeted (api plans, attempt 1) | 0/7 — Postgres connection-refused (26th VM reset mid-run, NOT a code failure) |
| P1-049 targeted (api plans, after restore) | 7/7 passed, ~18 s |
| P1-049 targeted (profile, after refactor) | 16/16 passed |
| P1-049 full verify | **green: 809 passed, 0 failed** (797 + 12 new), attempt 1, ~48 min, no flakes (log `.scratch/verify-p1-049.log`, pid 14310) |
| P1-049 drift checks | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 (logs `.scratch/drift-*.log`) |
| P1-049 lint-commits | ok: `0b29d77 feat(billing): P1-049 Plans service (FR-P-01)` |

## Resume state for the 38th coordinator

- **Branch:** `main` (checked out). **Main** is at
  `42e5677` (P1-049 merge). `phase-1` is at `0b29d77`
  (same content, merged). Nothing is pending push.
- **Current ticket:** P1-050 — Quota counters. Phase 1:
  49/111 (phase file + PROGRESS.md NOT yet updated for
  P1-050 — the 38th coordinator ticks them after the
  verify greens; the phase-file P1-050 boxes are still
  unticked and the current ticket is still P1-050).
- **P1-050 spec (from the phase file):** Type: feat,
  Area: billing/plans, Est: M, Deps: P1-049. Redis atomic
  daily counters (UTC reset) + usage_counters
  write-through; video-seconds estimator at job create;
  nightly reconciliation fixes drift. AC: UTC boundary
  clock-injected test.
- **P1-050 implementation (uncommitted, on `phase-1`):**
  `crates/core/src/usage.rs` + `usage_record.lua` +
  `usage_reconcile.lua` + `crates/core/tests/usage_live.rs`
  + `repos::usage_counters::{increment, overwrite}` +
  `crates/core/src/lib.rs` (`pub mod usage`) + 2 new
  `.sqlx/query-*.json` files + docs
  (test-strategy P1-050 section, data-model pointer,
  ASSUMPTION-016). Design decisions (also in the module
  docs + ASSUMPTION-016): Redis hash
  `pf:usage:{user}:{YYYY-MM-DD}` fields `images` /
  `video_seconds`, 48 h TTL, one Lua script
  (HINCRBY + EXPIRE + HMGET return); record order Redis
  first, then DB upsert (fail closed — a Redis failure
  writes no DB row); reconcile is DB-wins via a CAS Lua
  script (3 attempts, then `UsageError::Conflict`); the
  nightly SCHEDULE is P1-082's, not this ticket's; no
  production caller records usage yet (job-create is a
  501 stub) — wiring lands with job-create / P1-051.
  Boundary notes: P1-079 (E1.9) later adds the
  completion true-up; P1-082 the audited nightly job.
  Do NOT re-implement those here.
- **Full `just verify` RUNNING for P1-050:** pid 41218,
  log `.scratch/verify-p1-050.log` (launched ~17:00 IST,
  ~48 min expected). ADOPT IT BY LOG — never restart a
  running verify. When it greens: run the 6 drift checks,
  tick the P1-050 boxes + Status line in the phase file,
  update PROGRESS.md (50/111, current ticket P1-051,
  health-snapshot verify line, P1-050 note, next-up
  queue), commit ONE commit on `phase-1` as
  `feat(billing): P1-050 Quota counters (FR-P-02)` (also
  cite FR-P-07 in the body — the linter needs at least
  one FR/NFR reference; subject form `<type>(<scope>):`
  + ticket + description + `(FR-...)`), `just
  lint-commits`, merge `phase-1` into `main` (default
  subject, no rebase, NEVER push — sync is the driver's
  job), then continue to P1-051 in the same run if time
  remains.
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). If a reset hits,
  restore per ASSUMPTION-010 (26th reset now recorded
  there) and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed.
- **Working tree (uncommitted, on `phase-1`):** the
  P1-050 files listed above + `??` this night-report
  file. Do not discard any of them. `main` is at
  `42e5677` (P1-049 merge); `phase-1` is at `0b29d77`
  + the uncommitted P1-050 tree.

# NIGHT-REPORT-2026-11-15 — 38th coordinator (run ended by cron timeout, NOT by completion)

## What was done (final)

- **Adopted the running P1-050 verify by log — never restarted it.** At adoption (~17:00 IST) it was in the `cargo test --workspace` compile phase; rustc processes were actively compiling (genuinely progressing, not wedged). Note: the driver's "~71 min" estimate was wrong — pid 41219's elapsed time shows the verify launched ~16:55 IST, so at adoption it was only ~5 min in.
- **Verify progress at timeout (~17:39 IST): 340 passed, 0 failed, 48 test binaries done** — still running (pid 41219, elapsed 43:30, log `.scratch/verify-p1-050.log`). Expected total ~816 (809 + 7 new: 3 usage unit + 4 usage_live). No failures so far. This verify is running slower than P1-049's (~48 min); at the observed rate it needs roughly another 45–60 min of test execution.
- **Nothing committed, nothing merged, no drift checks run yet** — all of that waits on the verify going green. The P1-050 tree is untouched and intact (same uncommitted files as the 37th left).

## Verify results (final)

| Run | Result |
| --- | --- |
| P1-050 full verify (adopted) | **STILL RUNNING at timeout: 340 passed, 0 failed, 48 binaries** (pid 41219, log `.scratch/verify-p1-050.log`, ~43 min elapsed) |
| P1-050 targeted (37th, carried over) | usage unit 3/3, usage_live 4/4, fmt + clippy clean on detag-core |

## Resume state for the 39th coordinator

- **Branch:** `phase-1` (checked out). **Main** is at `42e5677` (P1-049 merge, pushed, in sync with remote — driver verified). `phase-1` is at `0b29d77` + the uncommitted P1-050 tree. Nothing pending push.
- **Current ticket:** P1-050 — Quota counters. Phase 1: 49/111.
- **Full `just verify` RUNNING for P1-050:** pid 41219 (`just verify`; the wrapper is pid 41218), log `.scratch/verify-p1-050.log`. ADOPT IT BY LOG — never restart it. When it greens: run the 6 drift checks, tick the P1-050 boxes + Status line in `docs/07-PHASES/PHASE-1-identity-billing.md`, update `docs/09-STATE/PROGRESS.md` (50/111, current ticket P1-051, health-snapshot verify line, P1-050 note, next-up queue), commit ONE commit on `phase-1` as `feat(billing): P1-050 Quota counters (FR-P-02)` (cite FR-P-07 in the body too), `just lint-commits`, merge `phase-1` into `main` (default subject, no rebase, NEVER push), then continue to P1-051 in the same run if time remains.
- **Environment:** services UP (postgres :5432, redis PONG). No VM reset hit this run. No open S1 issues. Open: S2 DISK-QUOTA-001, S3 TESTKIT-MINIO-001, S4 FLAKY-003.
- **Working tree (uncommitted, on `phase-1`):** `crates/core/src/usage.rs` + `usage_record.lua` + `usage_reconcile.lua` + `crates/core/tests/usage_live.rs` + `crates/core/src/lib.rs` + `crates/core/src/repos/usage_counters.rs` + 2 new `.sqlx/query-*.json` + docs (test-strategy P1-050 section, data-model pointer, ASSUMPTION-016) + this night-report file. Do not discard any of them.

---

# NIGHT-REPORT-2026-11-15 — 39th coordinator (run ended by cron timeout, NOT by completion)

## What was done (final)

- **P1-050 DONE + merged to main (locally):** adopted the
  38th coordinator's running `just verify` by log (pid
  41219, log `.scratch/verify-p1-050.log`) — do NOT restart
  was honoured; it finished green **816 passed, 0 failed**
  (809 + 7 new: 3 usage unit + 4 usage_live), attempt 1,
  ~50 min, no flakes ("verify OK: fmt + clippy + tests
  green"). Drift checks re-run with real exit codes, all 6
  green (openapi / route-table / config-table / env /
  sqlx-prepare / snapshots). Ticked the P1-050 boxes +
  Status line in the phase file; updated PROGRESS.md
  (50/111, current ticket P1-051, health-snapshot verify
  line, P1-050 note, next-up queue). Committed as one
  commit `aae13e7 feat(billing): P1-050 Quota counters
  (FR-P-02)` (13 files; the night-report file stays
  uncommitted, per convention), `just lint-commits` ok,
  merged `phase-1` into `main` as `4304fa1`. Phase 1 now
  **50/111**. Nothing is pending push (sync is the
  driver's job — NEVER push). No VM reset hit this run.
- **P1-051 IMPLEMENTED, NOT COMMITTED (verify running for
  the 40th coordinator to adopt):** the quota pre-check
  on `POST /v1/jobs` — new `apps/api/src/services/quota.rs`
  (`precheck`: load the upload, resolve the effective
  plan, read the durable snapshot, decide; check order
  quota dimensions first, then concurrency) + the
  `create_job` handler in `apps/api/src/routes/jobs.rs`
  (auth first, then the gate, then the 501 stub; the
  `X-Quota-*` headers stamped on the allowed 501 and on
  the 429 alike) + new core error `QuotaConcurrencyExceeded`
  → **PF-3008**, 429 (ADR-0026) + `jobs::count_running_for_user`
  (status `queued`/`running` occupy a slot; `held` does
  not) + `.sqlx/` regen (1 new query file) + new
  `apps/api/tests/quota_middleware.rs` (8 live tests) +
  the PF-3008 leg in `error_contract.rs`. Docs: api-spec
  quota paragraph, test-strategy P1-051 section,
  data-model jobs-row pointer, ASSUMPTION-017, ADR-0026.
  Targeted green: quota_middleware 8/8 (first run, ~69 s),
  error_contract 5/5, core error lib 17/17, fmt + clippy
  `-D warnings` clean. Two fixes while adopting: the
  error.rs count assertions (37 → 38 variants, in the
  all-variants list test + the exact-value table test)
  and a fmt drift in the new test file; one near-miss —
  an edit landed `count_running_for_user` in the middle
  of the `set_status` doc comment (caught by reading
  the section back, restored, re-inserted cleanly).
  NOT done: full `just verify` (RUNNING — see below),
  drift checks, phase-file tick + PROGRESS.md, commit,
  merge. That is the 40th coordinator's job — adopt the
  verify by log, do NOT restart it.

## Verify results (final)

| Run | Result |
| --- | --- |
| P1-050 full verify (38th's run, adopted by log) | **green: 816 passed, 0 failed**, attempt 1, ~50 min, no flakes |
| P1-050 drift checks (re-run, real exit codes) | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-050 lint-commits | ok: `aae13e7 feat(billing): P1-050 Quota counters (FR-P-02)` |
| P1-051 targeted (`quota_middleware`) | **8/8 passed**, first run, ~69 s |
| P1-051 targeted (`error_contract`) | **5/5 passed** (incl. the new PF-3008 leg) |
| P1-051 targeted (core error lib) | **17/17 passed** (after the 37 → 38 count fix) |
| P1-051 fmt + clippy `-D warnings` | clean |
| P1-051 full verify | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-050.log`,
`.scratch/verify-p1-051.log` (running).

## Resume state for the 40th coordinator

- **Branch:** `phase-1` (checked out). **Main** is at
  `4304fa1` (P1-050 merge). `phase-1` is at `aae13e7`
  (same content, merged). Nothing is pending push (sync
  is the driver's job — NEVER push).
- **Current ticket:** P1-051 — Quota middleware. Phase 1:
  50/111 (phase file + PROGRESS.md NOT yet updated for
  P1-051 — the 40th coordinator ticks them after the
  verify greens).
- **P1-051 full verify RUNNING:** `just verify`, pid
  72764 (launched ~19:23 IST via nohup, log
  `~/workspace/detag/.scratch/verify-p1-051.log`).
  Adopt it by log — do NOT restart it. Expected result:
  **~824 passed, 0 failed** (816 + 8 new quota_middleware
  tests). If it goes green: (1) run the drift checks
  (`just openapi-check`, `just route-table-check`, `just
  config-table-check`, `just env-check`,
  `just sqlx-prepare-check`, `just snapshots-check` — all
  must be green); (2) tick the P1-051 checkboxes + Status
  line in `docs/07-PHASES/PHASE-1-identity-billing.md`
  and update `docs/09-STATE/PROGRESS.md` (51/111, current
  ticket P1-052, health-snapshot verify line, P1-051
  note, next-up queue); (3) commit as one commit —
  suggested subject `feat(billing): P1-051 Quota
  middleware (FR-P-02)` (cite FR-P-06 + FR-P-07 in the
  body too; linter will confirm; run `just
  lint-commits`); (4) merge `phase-1` into `main`
  locally (plain merge, default subject, no rebase, no
  squash, NEVER push).
- **Then:** P1-052 — Quota headers everywhere (Type:
  feat, Area: billing/plans, Est: S, Deps: P1-051; spec:
  Job-creating routes attach remaining quota; snapshot
  in /v1/me; UI contract documented. AC: headers on all
  create routes).
- **P1-051 design (for the 40th — do NOT re-decide
  without cause):** the gate reads the DURABLE snapshot
  (`effective_plan` + `quota_snapshot_for_plan`), never
  the Redis hash — ASSUMPTION-017 + ADR-0026 record why
  (agreement with `/v1/me` by construction; the DB is
  what the suites seed). The check is read-only: no usage
  recorded, no slot acquired (job creation is still the
  501 stub). A video upload needs
  `max(estimate_video_seconds(duration), 1)` — an
  unknown-duration video on Free is PF-3002, not a free
  pass. Check order: images, then video-seconds, then
  concurrency. Concurrency denial is PF-3008 (new core
  variant; PF-3005 was NOT reused — system-wide queue
  signal, no upgrade hint). A missing or foreign
  `upload_id` is 404 PF-2010; no body / no `upload_id`
  reaches the stub untouched with no quota headers.
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). No VM reset hit this
  run. If a reset hits, restore per ASSUMPTION-010 and
  add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed. CI #27 + Commits #27 both SUCCESS
  (third consecutive fully-green run); CI #28 + Commits #28
  (on 3348ce3) were in progress at run start — results come
  via the user's logs zip, the coordinator cannot check GitHub
  (2FA SMS does not reach the user, Gmail not connected).
- **Working tree (uncommitted, on `phase-1`):** P1-051 —
  `M crates/core/src/error.rs`, `M
  crates/core/src/repos/jobs.rs`, `M
  crates/core/src/usage.rs` (doc sentence corrected),
  `?? .sqlx/query-4948d52773c7f32d365bdaf74190d30d6fcff497b30a961bf9cd1a0b907210eb.json`,
  `?? apps/api/src/services/quota.rs`, `M
  apps/api/src/services/mod.rs`, `M
  apps/api/src/routes/jobs.rs`, `?? 
  apps/api/tests/quota_middleware.rs`, `M
  apps/api/tests/error_contract.rs`, `M
  docs/03-ARCHITECTURE/03-api-spec.md`, `M
  docs/03-ARCHITECTURE/04-data-model.md`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/09-STATE/DECISIONS.md`, `M docs/09-STATE/ISSUES.md`,
  `??` this night-report file. Do not discard any of
  them. NOTE: the phase file + PROGRESS.md are NOT in
  this list — they were committed with P1-050 and are
  untouched for P1-051 until the verify greens.

---

# NIGHT-REPORT-2026-11-15 — 40th coordinator (run in progress at write time)

## What was done

- **Adopted the P1-051 verify by log — never restarted it.**
  At adoption (~18:08 IST) pid 72764 was in the `cargo test
  --workspace` compile phase, genuinely progressing. Note:
  the driver's "~1h37m elapsed" was stale — the process had
  in fact started 18:06 IST (12:36 UTC), ~2 min before
  adoption, so the verify was young, not nearly done.
  Progress at ~18:47 IST: 30 test-result lines, 0 failures,
  mid-metrics.rs — then the 27th VM reset killed it (see
  below). NOTHING was committed yet; the P1-051 tree is
  intact and untouched.
- **CI failure diagnosed (user-reported ~18:04 IST, logs zip
  `~/workspace/user/files/logs_118338230856_0_unkk.zip`):**
  CI run on `42e5677` (P1-049 merge) failed ONLY in the
  verify job — one test, `detag-api::key_attribution
  ::admin_jobs_list_auth_rules`, panicked at
  `key_attribution.rs:100` (login expected 200, got 423
  `PF-1008` account_locked). The other four jobs
  (cargo-deny, image-size, smoke, gitleaks) show zero
  FAIL lines. **Root cause (cross-checked twice): a
  cross-test IP-lockout collision through the shared
  250-bucket XFF pool.** `email_flow
  ::lockout_enqueues_one_notice_with_no_token` deliberately
  locks one IP bucket (5 failed logins, same XFF) and its
  `unique_xff()` draws from the shared `203.0.113.{1..250}`
  pool used by ~15 test files; the lock lasts 60 s
  (LOCKOUT_BASE_SECS). In CI it finished 11:46:10 UTC, so
  its lock ran until ~11:47:05–10; the key_attribution test
  ran 11:46:41–58 — inside the window — and one of its
  three logins drew the same bucket (1/250 per login) and
  got 423. Cross-check 2 (local): the harness DB held a
  locked IP row whose sha256 matches `203.0.113.233` — a
  bucket inside the shared pool, locked with attempts=5.
  The lockout service behaved correctly (a locked IP must
  423); the bug is test isolation — the P1-003 lesson
  ("each test gets its own IP bucket") is violated by the
  250-bucket pool. **Fix (not yet applied):** change
  `email_flow.rs` `unique_xff()` to the full-UUID form
  (`203.0.113.<uuid>`, opaque key — the lockout.rs /
  login.rs pattern), so the lock-creating test uses a
  bucket no other test can draw. It is the ONLY test that
  locks a bucket inside the shared pool (lockout.rs uses
  full-UUID xff; password_reset's 5-failure test sends no
  XFF, so its IP counter is skipped; login.rs rate-limit
  tests use UUID/no-XFF). Record as **CI-FAIL-009, S1** in
  ISSUES.md and fix it as its own commit BEFORE P1-052
  (after P1-051 lands) — the user asked for the fix in
  the next push. NOT yet in ISSUES.md — the 41st
  coordinator must add the entry with the fix commit.
- **27th VM reset (~18:47 IST, mid-P1-051 verify attempt
  1):** uptime 0 min at discovery; `just verify` pid
  72764 gone; the log ends mid-metrics.rs (30 binaries,
  0 failures), so the kill is proven. No os-intent replay
  was running; binaries gone entirely (no psql /
  redis-server, no `/etc/redis`, no `/var/lib/postgresql`,
  apt cache empty); `just` + cargo survived; the P1-051
  working tree survived intact (uncommitted, on
  `phase-1`). Restored per ASSUMPTION-010: `sudo apt-get
  update` (64.7 MB, 9m48s on the cogentco mirror; one
  noble-updates InRelease connection failure, same as
  prior resets) + `sudo apt-get install -y redis-server
  postgresql-16`; `pg_ctlcluster 16 main start` +
  setsid redis (PONG); `pf` role/db recreated via
  `sudo -u postgres psql`, password `pf` (grep-first
  extraction verified); `just migrate` applied all 17
  migrations. Update line for ASSUMPTION-010 NOT yet
  added to ISSUES.md — add it in the P1-051 commit (the
  27th reset, ~18:47 IST, same restore steps as the
  22nd/25th).
- **P1-051 verify RE-RUN from scratch (attempt 2):** the
  killed attempt 1 cannot be resumed (cargo test has no
  resume), and per the pace decision the ticket keeps a
  FULL verify. Launched ~19:00 IST via nohup (`just
  verify`, pid 6195), log
  `.scratch/verify-p1-051-attempt2.log` — in the compile
  phase at last check, genuinely progressing. Expected:
  **~824 passed, 0 failed** (816 + 8 new
  quota_middleware tests). The killed attempt 1's log
  (`.scratch/verify-p1-051.log`, 30 binaries, 0 failed)
  is kept for the record.

## Verify results

| Run | Result |
| --- | --- |
| P1-051 full verify, attempt 1 (39th's run, adopted by log) | KILLED by the 27th VM reset mid-metrics.rs — 30 test-result lines, 0 failures; log ends mid-binary, no result |
| P1-051 full verify, attempt 2 | **RUNNING at write time — adopt, do not restart** (see resume) |
| P1-051 targeted (39th, carried over) | quota_middleware 8/8, error_contract 5/5, core error lib 17/17, fmt + clippy clean |
| CI run on 42e5677 (user's logs zip) | verify job FAILED on 1 test (`admin_jobs_list_auth_rules`, 423 PF-1008); cargo-deny / image-size / smoke / gitleaks clean — root cause CI-FAIL-009, see above |

Logs: `.scratch/verify-p1-051.log` (killed attempt 1),
`.scratch/verify-p1-051-attempt2.log` (running).

## Resume state for the 41st coordinator

- **Branch:** `phase-1` (checked out). **Main** is at
  `4304fa1` (P1-050 merge, pushed by the driver —
  `last-pushed.txt` already reads `4304fa1`). Nothing is
  pending push (sync is the driver's job — NEVER push).
- **Current ticket:** P1-051 — Quota middleware. Phase 1:
  50/111 (phase file + PROGRESS.md NOT yet updated for
  P1-051 — tick them after the verify greens).
- **P1-051 full verify RUNNING:** `just verify`, pid 6195
  (started ~19:00 IST via nohup, attempt 2 after the
  27th reset killed attempt 1), log
  `~/workspace/detag/.scratch/verify-p1-051-attempt2.log`.
  Adopt it by log — do NOT restart it. Expected result:
  **~824 passed, 0 failed** (816 + 8 new quota_middleware
  tests). If it goes green: (1) run the drift checks
  (`just openapi-check`, `just route-table-check`, `just
  config-table-check`, `just env-check`,
  `just sqlx-prepare-check`, `just snapshots-check` — all
  must be green); (2) add the 27th VM-reset update line
  to ASSUMPTION-010 in `docs/09-STATE/ISSUES.md` (see
  above — ~18:47 IST, same restore steps as the 22nd/
  25th); (3) tick the P1-051 checkboxes + Status line in
  `docs/07-PHASES/PHASE-1-identity-billing.md` and update
  `docs/09-STATE/PROGRESS.md` (51/111, current ticket
  P1-052, health-snapshot verify line, P1-051 note,
  next-up queue); (4) commit as one commit — subject
  `feat(billing): P1-051 Quota middleware (FR-P-02)`
  (cite FR-P-06 + FR-P-07 in the body too; linter will
  confirm; run `just lint-commits`); (5) merge `phase-1`
  into `main` locally (plain merge, default subject, no
  rebase, no squash, NEVER push).
- **Then — NEW S1 FIRST, before P1-052:** record
  **CI-FAIL-009 (S1)** in `docs/09-STATE/ISSUES.md` and
  fix it as its own commit: change `unique_xff()` in
  `apps/api/tests/email_flow.rs` from the shared-pool
  form (`203.0.113.{Uuid::now_v7().as_u128() % 250 + 1}`)
  to the full-UUID form (`203.0.113.{Uuid::now_v7()}` —
  the lockout.rs pattern; the middleware treats the XFF
  first entry as an opaque key). Evidence for the root
  cause is in this report (CI timing + local DB proof:
  bucket 203.0.113.233 locked with attempts=5). Targeted
  re-run: `email_flow` + `key_attribution` binaries, then
  a FULL `just verify` for the fix commit (pace decision),
  commit `fix(tests): CI-FAIL-009 ...`, `just
  lint-commits`, merge to `main`. Only then start P1-052
  (Quota headers everywhere: job-creating routes attach
  remaining quota; snapshot in /v1/me; UI contract
  documented. AC: headers on all create routes).
- **P1-051 design (do NOT re-decide without cause):** the
  gate reads the DURABLE snapshot (`effective_plan` +
  `quota_snapshot_for_plan`), never the Redis hash —
  ASSUMPTION-017 + ADR-0026. Read-only: no usage recorded,
  no slot acquired (job creation is still the 501 stub).
  A video upload needs `max(estimate, 1)` video-seconds
  — unknown-duration video on Free is PF-3002. Check
  order: images, then video-seconds, then concurrency
  (PF-3008, ADR-0026). Missing/foreign `upload_id` is 404
  `PF-2010`; no body / no `upload_id` reaches the stub
  untouched with no quota headers.
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). The 27th reset is
  recorded above but NOT yet in ASSUMPTION-010 — see
  step (2).
- **Open issues:** NEW S1: CI-FAIL-009 (see above — not
  yet in ISSUES.md). S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed.
- **Working tree (uncommitted, on `phase-1`):** P1-051 —
  `M crates/core/src/error.rs`, `M
  crates/core/src/repos/jobs.rs`, `M
  crates/core/src/usage.rs` (doc sentence corrected),
  `?? .sqlx/query-4948d52773c7f32d365bdaf74190d30d6fcff497b30a961bf9cd1a0b907210eb.json`,
  `?? apps/api/src/services/quota.rs`, `M
  apps/api/src/services/mod.rs`, `M
  apps/api/src/routes/jobs.rs`, `??
  apps/api/tests/quota_middleware.rs`, `M
  apps/api/tests/error_contract.rs`, `M
  docs/03-ARCHITECTURE/03-api-spec.md`, `M
  docs/03-ARCHITECTURE/04-data-model.md`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/09-STATE/DECISIONS.md`, `M docs/09-STATE/ISSUES.md`,
  `??` this night-report file. Do not discard any of
  them. NOTE: the phase file + PROGRESS.md are NOT in
  this list — they were committed with P1-050 and are
  untouched for P1-051 until the verify greens.

---

# NIGHT-REPORT-2026-11-15 — 40th coordinator, closing (run ended by cron timeout, NOT by completion)

## What was done (final)

- **P1-051 verify attempt 2 went RED — root cause found
  and fixed in this run:** attempt 2 (pid 6195, after the
  27th-reset restore) stopped at `api_key_matrix` — 3
  passed, 1 failed. `api_key_matrix` panicked at
  `api_key_matrix.rs:405`: `POST /v1/jobs` with a
  full-scope key answered 422 `PF-2002` instead of the
  stub's 501 `PF-5004`. **Root cause:** the matrix probe
  sends an empty body WITH `content-type:
  application/json`; axum's `Option<Json<T>>` treats an
  empty body as "no body" only when there is NO JSON
  content-type — with the header, the `Json` extractor
  rejects the empty body ("EOF while parsing") and the
  handler never runs. The 39th's targeted tests only
  covered the route-table probe's shape (empty body, no
  content-type), so the gap was invisible until the full
  run. The handler's documented contract ("no body
  reaches the stub untouched") was therefore wrong for
  one of the two probe shapes — a product bug, not a
  test bug, so the fix is in the handler, not the
  matrix. **Fix (in the P1-051 tree, uncommitted):**
  `create_job` in `apps/api/src/routes/jobs.rs` now reads
  the body as raw `Bytes`: empty or whitespace-only →
  stub (both probe shapes identical); otherwise
  `serde_json::from_slice::<JobCreateRequest>` — a parse
  failure is `AppError::ValidationFailed` (422 `PF-2002`,
  byte-identical to what the `Json` extractor produced);
  no `upload_id` → stub; with one → the gate, unchanged.
  Module docs updated with the why (the `Option<Json>`
  trap) + api-spec sentence corrected to name both
  shapes. Regression tests in `quota_middleware.rs`: the
  no-body test now also covers the empty-body-WITH-JSON-
  content-type shape (501, no quota headers), and a new
  `malformed_json_body_is_422_pf2002` test pins the
  manual-parse mapping. Targeted green after the fix:
  quota_middleware **9/9** (8 + the new test), 
  api_key_matrix **4/4**, fmt + clippy `-D warnings`
  clean (one fmt drift in the new handler code, fixed
  with `cargo fmt`). Attempt 2's other 11 binaries were
  green before the stop (incl. quota_middleware 8/8 in
  the pre-fix run).
- **P1-051 verify attempt 3 RUNNING for the 41st
  coordinator to adopt** (see resume). The fix changed
  the handler, so the ticket keeps a FULL verify per the
  pace decision — no partial re-run.

## Verify results (final)

| Run | Result |
| --- | --- |
| P1-051 full verify, attempt 1 | KILLED by the 27th VM reset mid-metrics.rs — 30 binaries, 0 failures |
| P1-051 full verify, attempt 2 | **RED: api_key_matrix 1 failed** (`POST /v1/jobs` 422 vs 501 — the `Option<Json>` empty-body trap); 11 binaries green before the stop |
| P1-051 fix targeted (`quota_middleware` + `api_key_matrix`) | **quota_middleware 9/9, api_key_matrix 4/4**, fmt + clippy clean |
| P1-051 full verify, attempt 3 | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-051.log` (killed attempt 1),
`.scratch/verify-p1-051-attempt2.log` (red),
`.scratch/verify-p1-051-attempt3.log` (running).

## Resume state for the 41st coordinator

- **Branch:** `phase-1` (checked out). **Main** is at
  `4304fa1` (P1-050 merge, pushed by the driver).
  Nothing is pending push (sync is the driver's job —
  NEVER push).
- **Current ticket:** P1-051 — Quota middleware. Phase 1:
  50/111 (phase file + PROGRESS.md NOT yet updated for
  P1-051 — tick them after the verify greens).
- **P1-051 full verify RUNNING:** `just verify`, pid
  27560 (started ~19:31 IST via nohup, attempt 3 — after
  the empty-body fix), log
  `~/workspace/detag/.scratch/verify-p1-051-attempt3.log`.
  Adopt it by log — do NOT restart it. Expected result:
  **~825 passed, 0 failed** (816 + 9 new
  quota_middleware tests). If it goes green: (1) run the
  drift checks (`just openapi-check`, `just
  route-table-check`, `just config-table-check`, `just
  env-check`, `just sqlx-prepare-check`, `just
  snapshots-check` — all must be green); (2) add the 27th
  VM-reset update line to ASSUMPTION-010 in
  `docs/09-STATE/ISSUES.md` (~18:47 IST, same restore
  steps as the 22nd/25th); (3) tick the P1-051
  checkboxes + Status line in
  `docs/07-PHASES/PHASE-1-identity-billing.md` and update
  `docs/09-STATE/PROGRESS.md` (51/111, current ticket
  P1-052, health-snapshot verify line, P1-051 note,
  next-up queue) — the P1-051 note must mention the
  empty-body fix (handler reads raw bytes; the matrix
  probe's empty-body-with-JSON-content-type shape is now
  covered by a regression test); (4) commit as one commit
  — subject `feat(billing): P1-051 Quota middleware
  (FR-P-02)` (cite FR-P-06 + FR-P-07 in the body too;
  run `just lint-commits`); (5) merge `phase-1` into
  `main` locally (plain merge, default subject, no
  rebase, no squash, NEVER push).
- **Then — NEW S1 FIRST, before P1-052:** record
  **CI-FAIL-009 (S1)** in `docs/09-STATE/ISSUES.md` and
  fix it as its own commit: change `unique_xff()` in
  `apps/api/tests/email_flow.rs` from the shared-pool
  form (`203.0.113.{Uuid::now_v7().as_u128() % 250 + 1}`)
  to the full-UUID form (`203.0.113.{Uuid::now_v7()}` —
  the lockout.rs pattern; the middleware treats the XFF
  first entry as an opaque key). Evidence for the root
  cause is in the 40th's section above (CI timing + local
  DB proof: bucket 203.0.113.233 locked with attempts=5).
  Targeted re-run: `email_flow` + `key_attribution`
  binaries, then a FULL `just verify` for the fix commit
  (pace decision), commit `fix(tests): CI-FAIL-009 ...`,
  `just lint-commits`, merge to `main`. Only then start
  P1-052 (Quota headers everywhere: job-creating routes
  attach remaining quota; snapshot in /v1/me; UI contract
  documented. AC: headers on all create routes).
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). The 27th reset is
  recorded above but NOT yet in ASSUMPTION-010 — see
  step (2).
- **Open issues:** NEW S1: CI-FAIL-009 (see above — not
  yet in ISSUES.md). S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed.
- **Working tree (uncommitted, on `phase-1`):** P1-051 +
  the empty-body fix — `M crates/core/src/error.rs`, `M
  crates/core/src/repos/jobs.rs`, `M
  crates/core/src/usage.rs`, `??
  .sqlx/query-4948d52773c7f32d365bdaf74190d30d6fcff497b30a961bf9cd1a0b907210eb.json`,
  `?? apps/api/src/services/quota.rs`, `M
  apps/api/src/services/mod.rs`, `M
  apps/api/src/routes/jobs.rs` (fix: raw-Bytes body),
  `?? apps/api/tests/quota_middleware.rs` (9 tests incl.
  the malformed-JSON regression), `M
  apps/api/tests/error_contract.rs`, `M
  docs/03-ARCHITECTURE/03-api-spec.md`, `M
  docs/03-ARCHITECTURE/04-data-model.md`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/09-STATE/DECISIONS.md`, `M docs/09-STATE/ISSUES.md`,
  `??` this night-report file. Do not discard any of
  them.

---

# NIGHT-REPORT-2026-11-15 — 40th coordinator, correction (read before the resume below)

## Correction: the CI failure is ALREADY fixed on main — do NOT re-do it

- **The CI-FAIL-009 fix I planned above is DONE, as
  FLAKY-006, commit `8677bd9` on `main`** (authored
  18:36 IST today by another Detag agent, `test(api):
  FLAKY-006 unique test XFFs stop IP-lockout collisions
  (NFR-52)`). It is the BROADER fix: all 28 test files'
  `unique_xff()` changed from the `% 250` pool to the
  full-UUID form, + a FLAKY-006 entry in ISSUES.md. Its
  root-cause analysis matches mine exactly (same producer
  test, same 48 s gap, same CI #36). The "record CI-FAIL-009
  in ISSUES.md and fix it as its own commit BEFORE P1-052"
  instruction in the resume below is therefore **STALE —
  do NOT create a CI-FAIL-009 entry and do NOT write a
  separate fix commit.** The S1 is FLAKY-006, fixed on
  main, awaiting CI #37 confirmation on push (the driver
  pushes; the fix commit was verified fmt/clippy-clean +
  test binaries compile in a detached worktree, deliberately
  no local test run — it would have shared the scratch DB
  with the running P1-051 verify).
- **One loose end the 41st MUST handle at merge time
  (called out in the FLAKY-006 entry itself):**
  `apps/api/tests/quota_middleware.rs` — new in P1-051,
  on `phase-1` only — still carries the old `% 250`
  `unique_xff()` (line 83-84). When the P1-051 commit
  lands and `phase-1` merges into `main`, apply the same
  one-line change there (`203.0.113.{Uuid::now_v7()}`).
  Do NOT apply it to the running verify's tree now (the
  attempt-3 binaries are already compiled; the change is
  test-only and identical to the 28 already-applied lines
  — apply it as part of the P1-051 commit or the merge,
  then re-run the `quota_middleware` binary targeted to
  confirm the merged tree).
- **Expected ISSUES.md merge conflict:** `main`'s
  ISSUES.md has the FLAKY-006 entry; `phase-1`'s will
  add the ASSUMPTION-010 27th-reset line in the P1-051
  commit. When merging `phase-1` into `main`, resolve by
  keeping BOTH — do not drop either entry.
- **P1-051's expected verify count is unchanged** (~825):
  the FLAKY-006 change is not in the `phase-1` tree the
  attempt-3 verify runs against, and it touches no
  product code.


---

# NIGHT-REPORT-2026-11-15 — 42nd coordinator (run ended by cron timeout, NOT by completion)

## What was done

- **P1-051 DONE + merged to main (locally):** adopted the
  41st coordinator's running `just verify` attempt 3 by log
  (pid 27560, log `.scratch/verify-p1-051-attempt3.log`) —
  do NOT restart was honoured. It finished green **825
  passed, 0 failed** (816 + 9 new quota_middleware tests),
  attempt 3, ~55 min, no flakes ("verify OK: fmt + clippy +
  tests green"). Drift checks re-run with real exit codes,
  all 6 green (openapi / route-table / config-table / env /
  sqlx-prepare / snapshots — logs
  `.scratch/drift-p1-051-*.log`). FLAKY-006 loose end
  closed in the commit: `quota_middleware.rs`
  `unique_xff()` is now the full-UUID form (the one-line
  change the 40th's correction prescribed; re-run
  quota_middleware 9/9 after the change — the 40th's
  correction explicitly sanctions the targeted re-run
  instead of a full re-verify for this test-only line,
  identical to the 28 already-applied lines). Also in the
  commit: the 27th VM-reset update line for ASSUMPTION-010,
  the P1-005 snapshot-agreement AC ticked (verified by
  P1-051), and the phase-file tick + PROGRESS.md (51/111,
  current ticket P1-052, health-snapshot verify line,
  P1-051 note, next-up queue). Committed as one commit
  `5ada0d6 feat(billing): P1-051 Quota middleware (FR-P-02)`
  (16 files; the night-report file stays uncommitted, per
  convention), `just lint-commits` ok, merged `phase-1`
  into `main` as `408e41c` — clean, no conflicts
  (ISSUES.md auto-merged: FLAKY-006 entry from main's side
  + ASSUMPTION-010 27th-reset line + ASSUMPTION-017 from
  phase-1's side, all present — verified by grep). Phase 1
  now **51/111**. Nothing is pending push (sync is the
  driver's job — NEVER push).
- **S1 check at run start: 0 open** (re-checked
  `docs/09-STATE/ISSUES.md` directly: CI-FAIL-001..020
  all fixed/closed; FLAKY-006 fixed on main as 8677bd9,
  awaiting CI #37. The 40th's planned CI-FAIL-009 entry
  is STALE — do NOT create it; the 40th's correction
  says so explicitly). Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike).
- **P1-052 IMPLEMENTED, NOT COMMITTED (verify running
  for the 43rd coordinator to adopt):** every job-creating
  response carries the `X-Quota-*` headers — new
  `apps/api/tests/quota_headers.rs` (8 live tests) +
  `retry_job` handler in `apps/api/src/routes/jobs.rs` +
  `quota::snapshot_for_user` (effective plan + durable
  snapshot, same two sources as the gate and `/v1/me`).
  Scope decisions (ASSUMPTION-018): the no-body /
  no-`upload_id` create path carries the caller's headers
  (closing P1-051's deliberate carve-out); retry carries
  the CALLER's headers and reads NO job row — an unknown
  id still 501s, the 404 ownership proof lands with the
  real retry handler in Phase 2 (loading the job now
  just to 404 would churn route-table / api-key-matrix /
  idor-sweep for a stub that still returns 501);
  `POST /v1/admin/jobs/{id}/reprocess` carries none
  (quota subject is the job owner, not the admin; the
  owner's remaining quota is in the admin jobs browse
  view, P1-038). Stub-path headers agree with the
  `/v1/me` snapshot (the gated path's agreement was
  proven in P1-051). New
  `docs/06-DELIVERY/08-quota-ui-contract.md` (headers,
  /v1/me snapshot shape, 429 rendering, seeded limits —
  Free 10/0/1, Pro 500/3600/3, Business 5000/18000/10,
  read from the seed in `crates/core/src/plans.rs`).
  Two fixes while implementing: clippy E0308 (tail
  expression needs `?` + `Ok(...)` — a tail `Result`
  does not auto-convert error types) and clippy
  `doc-lazy-continuation` (a doc line starting with
  "label:" makes the continuation a list item — blank
  line after the first line); plus one fmt drift and a
  missing `.body(Body::empty())` on the GET builder in
  the new test file. One expected failure while adopting:
  the P1-051 test `no_body_or_no_upload_id_still_reaches_stub`
  pinned the headers' ABSENCE — updated to the new
  contract (headers `10`/`0` on all three probe shapes)
  + its module-doc bullet; also fixed the module doc's
  stale "ten tests" (the suite has nine) and the
  test-strategy P1-051 line's stale "8 live tests" (nine).
  Targeted green: quota_headers 8/8, quota_middleware
  9/9, route_table 3/3, api_key_matrix 4/4, idor_sweep
  3/3, error_contract 5/5, fmt + clippy `-D warnings`
  clean. NOT done: full `just verify` (RUNNING — see
  below), drift checks, commit, merge. That is the 43rd
  coordinator's job — adopt the verify by log, do NOT
  restart it.
- **28th VM reset (~20:46 IST, mid-P1-052 clippy):**
  the runtime restarted while two dispatched commands ran
  (both aborted); uptime 0 min at discovery. No os-intent
  replay was running; binaries gone entirely (no psql /
  redis-server, no `/etc/redis`, no `/var/lib/postgresql`);
  `just` + cargo survived. Self-installed: `apt-get
  update` (~18 min crawling on the cogentco mirror) +
  `apt-get install -y redis-server postgresql-16` (both
  exit 0); `pg_ctlcluster 16 main start` + setsid redis
  (PONG); `pf` role/db recreated via `sudo -u postgres
  psql`, password `pf` (grep-first extraction verified);
  `just migrate` applied all 17 migrations. Update line
  added to ASSUMPTION-010 in the P1-052 tree. The P1-052
  working tree survived intact (uncommitted, on
  `phase-1` — note: the reset hit while on `main` after
  the P1-051 merge; the branch is still `main` at run
  end — the 43rd must `git checkout phase-1` first,
  the P1-052 tree is there).

## Verify results

| Run | Result |
| --- | --- |
| P1-051 full verify, attempt 3 (41st's run, adopted by log) | **green: 825 passed, 0 failed**, ~55 min, no flakes |
| P1-051 drift checks (re-run, real exit codes) | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-051 lint-commits | ok: `5ada0d6 feat(billing): P1-051 Quota middleware (FR-P-02)` |
| P1-051 XFF fix targeted (`quota_middleware`) | **9/9 passed** after the full-UUID change |
| P1-052 targeted (`quota_headers`) | **8/8 passed**, first run, ~55 s |
| P1-052 targeted (neighbors) | quota_middleware 9/9, route_table 3/3, api_key_matrix 4/4, idor_sweep 3/3, error_contract 5/5 |
| P1-052 fmt + clippy `-D warnings` | clean (after the E0308 + doc-lint + fmt fixes) |
| P1-052 full verify | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-051-attempt3.log` (green),
`.scratch/drift-p1-051-*.log`, `.scratch/fmt-p1-051-xff.log`,
`.scratch/p1-051-quota-xff.log`, `.scratch/verify-p1-052.log`
(running), `.scratch/restore-28-apt-update.log`,
`.scratch/restore-28-apt-install.log`,
`.scratch/restore-28-migrate.log`, `.scratch/fmt-p1-052.log`,
`.scratch/clippy-p1-052.log`, `.scratch/p1-052-targeted.log`,
`.scratch/p1-052-neighbors.log`,
`.scratch/p1-052-neighbors2.log`.

## Resume state for the 43rd coordinator

- **Branch:** `main` (checked out). **Main** is at
  `408e41c` (P1-051 merge). `phase-1` is at `5ada0d6`
  (same content, merged) + the uncommitted P1-052 tree.
  **`git checkout phase-1` FIRST** — the 42nd ended on
  `main`. Nothing is pending push (sync is the driver's
  job — NEVER push).
- **Current ticket:** P1-052 — Quota headers everywhere.
  Phase 1: 52/111 (phase file + PROGRESS.md already
  pre-updated, Status says the verify is PENDING — fill
  in the real numbers after the verify greens).
- **P1-052 full verify RUNNING:** `just verify`, pid
  11395 (launched ~21:21 IST via setsid nohup, attempt 1),
  log `~/workspace/detag/.scratch/verify-p1-052.log`.
  Adopt it by log — do NOT restart it. Expected result:
  **~833 passed, 0 failed** (825 + 8 new quota_headers
  tests). If it goes green: (1) run the drift checks
  (`just openapi-check`, `just route-table-check`, `just
  config-table-check`, `just env-check`,
  `just sqlx-prepare-check`, `just snapshots-check` — all
  must be green; P1-052 touches no routes/config/
  migrations, but CI parity requires the check); (2) fill
  the real verify numbers into the P1-052 Status line in
  `docs/07-PHASES/PHASE-1-identity-billing.md` (it
  currently says PENDING) and the PROGRESS.md P1-052 note
  (currently says the verify is running) — the health
  snapshot verify line still says 825, update it to the
  P1-052 count; (3) commit as one commit — suggested
  subject `feat(billing): P1-052 Quota headers everywhere
  (FR-P-02)` (linter will confirm; run `just
  lint-commits`); (4) merge `phase-1` into `main`
  locally (plain merge, default subject, no rebase, no
  squash, NEVER push).
- **Then:** P1-053 — Concurrency slots (Type: feat,
  Area: billing/plans, Est: S, Deps: P1-052; spec:
  Per-user running-jobs cap per plan; crash-safe slot
  acquire/release with TTL; slot-leak recovery tested.
  AC: leak recovery green).
- **P1-052 design (for the 43rd — do NOT re-decide
  without cause):** headers on every job-creating
  response (`POST /v1/jobs` all paths + retry, caller's
  snapshot via `snapshot_for_user`); retry reads no job
  row (ASSUMPTION-018 — the 404 lands with the real
  retry handler in Phase 2); reprocess carries none
  (owner is the quota subject, not the admin —
  ASSUMPTION-018); UI contract is
  `docs/06-DELIVERY/08-quota-ui-contract.md`.
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). If a reset hits,
  restore per ASSUMPTION-010 (28th reset now recorded
  there) and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004
  fixed, FLAKY-005 fixed, FLAKY-006 fixed on main
  (8677bd9, awaiting CI #37). CI #37 results come via
  the user's logs zip, the coordinator cannot check GitHub
  (2FA SMS does not reach the user, Gmail not connected).
- **Working tree (uncommitted, on `phase-1`):** P1-052 —
  `M apps/api/src/routes/jobs.rs`, `M
  apps/api/src/services/quota.rs`, `M
  apps/api/tests/quota_middleware.rs`, `??
  apps/api/tests/quota_headers.rs`, `??
  docs/06-DELIVERY/08-quota-ui-contract.md`, `M
  docs/03-ARCHITECTURE/03-api-spec.md`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/07-PHASES/PHASE-1-identity-billing.md`, `M
  docs/09-STATE/ISSUES.md`, `M docs/09-STATE/PROGRESS.md`,
  `??` this night-report file. Do not discard any of
  them. NOTE: the branch is `main` at run end — checkout
  `phase-1` first (see above).

---

# NIGHT-REPORT-2026-11-15 — 43rd coordinator (run ended by cron timeout, NOT by completion)

## What was done

- **Adopted the 42nd's running P1-052 verify by log — never restarted it.** At adoption (~21:31 IST) pid 11397 was ~11 min in, 8 binaries green, 0 failed, mid-admin_search.
- **29th VM reset (~22:26 IST, mid-verify attempt 1):** the runtime restarted while the 43rd waited on the verify — the log ends mid-`privileged_audit` (36 binaries green, 0 failed), so the kill is proven, not guessed. The os-intent replay (pid 764) ran ~20 min and DID restore the postgres/redis binaries itself this time. Restored per ASSUMPTION-010 (`pg_ctlcluster 16 main start` + setsid redis, PONG; `pf` role/db recreated, password `pf`; `just migrate`, all 17 migrations applied cleanly). `just` + cargo survived. The P1-052 working tree survived intact (uncommitted, on `main` — branch still `main` at run end; the 44th must `git checkout phase-1` first). Verify re-run from scratch as attempt 2 (pid 11487, log `.scratch/verify-p1-052-attempt2.log`).
- **30th VM reset (~23:16 IST, mid-verify attempt 2):** the runtime restarted again — the log ends mid-profile tests (37 binaries green, 0 failed), kill proven. This time the os-intent replay ran ~20 min and restored ONLY `psql` (redis-server / pg_ctlcluster still missing, no `/etc/redis`). Self-installed: `sudo apt-get update` (37.2 MB, 1m18s — fast this time) + `sudo apt-get install -y redis-server postgresql-16` (exit 0, logs `.scratch/restore-30-apt-update.log` / `.scratch/restore-30-apt-install.log`). Restored per ASSUMPTION-010 (`pg_ctlcluster 16 main start` + setsid redis, PONG; `pf` role/db recreated; `just migrate`, all 17 migrations applied cleanly, log `.scratch/restore-30-migrate.log`). The P1-052 working tree survived intact again. Verify re-run from scratch as attempt 3 (pid 11110, log `.scratch/verify-p1-052-attempt3.log`).
- **Nothing committed, nothing merged, no drift checks run yet** — all of that waits on the verify going green. The P1-052 tree is untouched and intact (same uncommitted files as the 42nd left). ASSUMPTION-010 update lines for the 29th/30th resets are NOT yet added to ISSUES.md — add them in the P1-052 commit.

## Verify results

| Run | Result |
| --- | --- |
| P1-052 full verify, attempt 1 (42nd's run, adopted by log) | KILLED by the 29th VM reset mid-privileged_audit — 36 test-result lines, 0 failures; log ends mid-binary, no result |
| P1-052 full verify, attempt 2 | KILLED by the 30th VM reset mid-profile — 37 test-result lines, 0 failures; log ends mid-binary, no result |
| P1-052 full verify, attempt 3 | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-052.log` (killed attempt 1), `.scratch/verify-p1-052-attempt2.log` (killed attempt 2), `.scratch/verify-p1-052-attempt3.log` (running), `.scratch/restore-30-apt-update.log`, `.scratch/restore-30-apt-install.log`, `.scratch/restore-30-migrate.log`.

## Resume state for the 44th coordinator

- **Branch:** `main` (checked out). **Main** is at `408e41c` (P1-051 merge). `phase-1` is at `5ada0d6` (same content, merged) + the uncommitted P1-052 tree. **`git checkout phase-1` FIRST** — the 43rd ended on `main`. Nothing is pending push (sync is the driver's job — NEVER push).
- **Current ticket:** P1-052 — Quota headers everywhere. Phase 1: 52/111 (phase file + PROGRESS.md already pre-updated, Status says the verify is PENDING — fill in the real numbers after the verify greens).
- **P1-052 full verify RUNNING:** `just verify`, pid 11110 (launched ~23:39 IST via setsid nohup, attempt 3; attempts 1–2 were each killed by a VM reset, 0 failures in both). At run end (~00:01 IST) it was 28 binaries green, 0 failed, mid-login tests. Log `~/workspace/detag/.scratch/verify-p1-052-attempt3.log`. Adopt it by log — do NOT restart it. Expected result: **~833 passed, 0 failed** (825 + 8 new quota_headers tests). If it goes green: (1) run the drift checks (`just openapi-check`, `just route-table-check`, `just config-table-check`, `just env-check`, `just sqlx-prepare-check`, `just snapshots-check` — all must be green); (2) add the 29th + 30th VM-reset update lines to ASSUMPTION-010 in `docs/09-STATE/ISSUES.md` (29th ~22:26 IST, replay restored binaries, same restore steps; 30th ~23:16 IST, replay restored only psql, self-installed via apt, same restore steps); (3) fill the real verify numbers into the P1-052 Status line in `docs/07-PHASES/PHASE-1-identity-billing.md` (it currently says PENDING) and the PROGRESS.md P1-052 note (currently says the verify is running) — the health snapshot verify line still says 825, update it to the P1-052 count; (4) commit as one commit — suggested subject `feat(billing): P1-052 Quota headers everywhere (FR-P-02)` (linter will confirm; run `just lint-commits`); (5) merge `phase-1` into `main` locally (plain merge, default subject, no rebase, no squash, NEVER push).
- **Then:** P1-053 — Concurrency slots (Type: feat, Area: billing/plans, Est: S, Deps: P1-052; spec: Per-user running-jobs cap per plan; crash-safe slot acquire/release with TTL; slot-leak recovery tested. AC: leak recovery green). Design notes gathered by the 43rd (do NOT re-decide without cause): P1-051's `precheck` already counts running jobs from the DB (`jobs::count_running_for_user`, `queued`/`running` occupy a slot, `held` does not) and denies with PF-3008 when `running >= plan.concurrency` (Free 1 / Pro 3 / Business 10, from `crates/core/src/plans.rs`); P1-053 must add the crash-safe acquire/release with TTL + leak recovery (Redis Lua pattern per `crates/core/src/usage.rs` / `usage_record.lua`). The pre-check stays read-only until real job creation lands in Phase 2.
- **Environment:** services are UP (postgres :5432 accepting, redis PONG, `just` + cargo on PATH via `~/.local/bin` + `~/.cargo/bin`). No VM reset since the 30th (uptime 45 min at run end). If a reset hits, restore per ASSUMPTION-010 and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3 TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) — FLAKY-004 fixed, FLAKY-005 fixed, FLAKY-006 fixed on main (8677bd9, awaiting CI #37). CI #37 results come via the user's logs zip, the coordinator cannot check GitHub (2FA SMS does not reach the user, Gmail not connected).
- **Working tree (uncommitted, on `phase-1`):** P1-052 — `M apps/api/src/routes/jobs.rs`, `M apps/api/src/services/quota.rs`, `M apps/api/tests/quota_middleware.rs`, `?? apps/api/tests/quota_headers.rs`, `?? docs/06-DELIVERY/08-quota-ui-contract.md`, `M docs/03-ARCHITECTURE/03-api-spec.md`, `M docs/05-QUALITY/01-test-strategy.md`, `M docs/07-PHASES/PHASE-1-identity-billing.md`, `M docs/09-STATE/ISSUES.md`, `M docs/09-STATE/PROGRESS.md`, `??` this night-report file. Do not discard any of them. NOTE: the branch is `main` at run end — checkout `phase-1` first (see above).

---

# NIGHT-REPORT-2026-11-15 — 44th coordinator (run ended by cron timeout, NOT by completion)

## What was done

- **P1-052 DONE + merged to main (locally):** adopted the
  42nd coordinator's uncommitted tree (new
  `apps/api/tests/quota_headers.rs` + `retry_job` handler
  + `quota::snapshot_for_user`, ASSUMPTION-018, the
  quota UI contract doc) and ran the full verify from
  scratch — the three earlier verify logs
  (`.scratch/verify-p1-052*.log`, attempts 1–3) all end
  mid-run with no result, so none was adoptable. Attempt
  4 green **833 passed, 0 failed** (825 + 8 new
  quota_headers tests), ~44 min, no flakes (log
  `.scratch/verify-p1-052-attempt4.log`). Drift checks
  green (openapi / route-table / config-table / env /
  sqlx-prepare / snapshots — logs
  `.scratch/drift-p1-052-*.log`). Cross-checked the
  diff while the verify ran: the code matches
  ASSUMPTION-018 (headers on create + retry, none on
  reprocess), and both new/updated test files use the
  full-UUID `unique_xff()` form (the FLAKY-006 loose
  end is closed in-tree). Committed as one commit
  `69a00b8 feat(billing): P1-052 Quota headers
  everywhere (FR-P-02)` (10 files; the night-report
  file stays uncommitted, per convention), `just
  lint-commits` ok, merged `phase-1` into `main` as
  `7fd6029`. Phase 1 now **52/111**. Nothing is pending
  push (sync is the driver's job — NEVER push).
- **Branch dance (recorded so the next coordinator
  does not trip on it):** the tree was dirty on `main`
  but `phase-1` was behind it — `main` carries the
  FLAKY-006 fix commit `8677bd9` (28 test files +
  ISSUES.md entry) which `phase-1` does not have, so a
  plain `git checkout phase-1` refused (ISSUES.md would
  be overwritten). Fixed with the stash dance: `git
  stash push` → `checkout phase-1` → `git stash pop`
  (ISSUES.md auto-merged cleanly) → commit on
  `phase-1` → merge into `main`. Post-merge ISSUES.md
  verified to hold BOTH the FLAKY-006 entry and
  ASSUMPTION-018. The same dance will be needed again
  whenever `main` gains a commit `phase-1` lacks.
- **Brief correction (NOT a file error):** the task
  brief said to update PROGRESS.md to 53/111 with
  current ticket P1-054 after committing P1-052 — that
  is off by one. The files are right: 51 done + P1-052
  = **52/111**, current ticket **P1-053**. The
  uncommitted tree already said exactly that, and it
  was kept, not bumped.
- **P1-053 IMPLEMENTED (not committed):** Concurrency
  slots — new `crates/core/src/slots.rs` (`SlotPool`:
  acquire/release/renew/recover/held over the Redis
  sorted set `pf:slots:{user}`, member = job id, score
  = lease-expiry epoch-ms, 1 h lease) + three Lua
  scripts (`slots_acquire.lua` — prune + idempotent
  re-acquire + cap check in one script;
  `slots_renew.lua`; `slots_held.lua`) + `slots_key` /
  `Namespace::Slots` / `TTL_SLOT_LEASE` in
  `crates/core/src/redis/keys.rs` + new
  `crates/core/tests/slots_live.rs` (9 live tests).
  Design decisions (module docs + ASSUMPTION-019):
  the 1 h lease matches the `worker_heartbeats`
  staleness prune horizon; the pool is Redis-only (no
  new table — the durable running count already exists
  in P1-051); no production caller yet — Phase 2
  intake wires acquire at job create and release at
  completion, and the P1-051 pre-check stays
  DB-count-based until then (a Redis-only gate now
  would never trip, the set being empty). Targeted
  green: slots_live 9/9 (incl. the AC
  `expired_leases_are_reclaimed`, the 10-way race
  admitting exactly 3, `renew_extends_the_lease`),
  slots unit 2/2, keys unit 5/5, fmt + clippy clean on
  detag-core. Docs: api-spec P1-053 sentence,
  test-strategy P1-053 section, data-model jobs-row
  pointer, ASSUMPTION-019 (verified by grep to sit
  after ASSUMPTION-018, not under another entry).
  Phase file ticked + PROGRESS.md pre-updated (53/111,
  current ticket P1-054) with the verify PENDING —
  fill in the real numbers after the verify below
  greens. Full `just verify` is RUNNING — see below.
- **No VM reset hit this run.** Services were up at
  run start (redis PONG, postgres accepting) and
  stayed up. No open S1 issues. Open: S2
  DISK-QUOTA-001, S3 TESTKIT-MINIO-001, S4 FLAKY-003
  (one strike) — FLAKY-004 fixed, FLAKY-005 fixed,
  FLAKY-006 fixed.

## Verify results

| Run | Result |
| --- | --- |
| P1-052 full verify, attempt 4 | **green: 833 passed, 0 failed**, ~44 min, no flakes |
| P1-052 drift checks | openapi / route-table / config-table / env / sqlx-prepare / snapshots — all exit 0 |
| P1-052 lint-commits | ok: `69a00b8 feat(billing): P1-052 Quota headers everywhere (FR-P-02)` |
| P1-052 targeted (42nd, carried over) | quota_headers 8/8, quota_middleware 9/9, route_table 3/3, api_key_matrix 4/4, idor_sweep 3/3, error_contract 5/5 |
| P1-053 targeted (`slots_live`) | **9/9 passed**, first run |
| P1-053 targeted (core unit) | slots 2/2, keys 5/5; fmt + clippy clean on detag-core |
| P1-053 full verify | **RUNNING at run end — adopt, do not restart** (see resume) |

Logs: `.scratch/verify-p1-052-attempt4.log`,
`.scratch/drift-p1-052-*.log`,
`.scratch/verify-p1-053.log` (running).

## Resume state for the 45th coordinator

- **Branch:** `phase-1` (checked out). **Main** is at
  `7fd6029` (P1-052 merge). Nothing is pending push
  (sync is the driver's job — NEVER push).
- **Current ticket:** P1-053 — Concurrency slots.
  Phase 1: 53/111 (phase file + PROGRESS.md already
  updated).
- **P1-053 full verify RUNNING:** `just verify`, pid
  26599 (wrapper pid 26598, launched ~00:36 IST via
  setsid nohup), log
  `~/workspace/detag/.scratch/verify-p1-053.log`.
  Adopt it by log — do NOT restart it. Expected result:
  **844 passed, 0 failed** (833 + 11 new: 9
  slots_live + 2 slots unit). If it goes green: (1)
  run the drift checks (`just openapi-check`,
  `just route-table-check`, `just config-table-check`,
  `just env-check`, `just sqlx-prepare-check`,
  `just snapshots-check` — all must be green; P1-053
  touches no routes/config/migrations, but CI parity
  requires the check); (2) fill the real verify
  numbers into the P1-053 Status line in
  `docs/07-PHASES/PHASE-1-identity-billing.md` (it
  currently says PENDING) and the PROGRESS.md
  health-snapshot verify line + P1-053 note
  (currently say the verify is running); (3) commit as
  one commit — suggested subject `feat(billing):
  P1-053 Concurrency slots (FR-P-02)` (linter will
  confirm; run `just lint-commits`); (4) merge
  `phase-1` into `main` locally (plain merge, default
  subject, no rebase, no squash, NEVER push). NOTE on
  the merge: if `main` has gained a commit `phase-1`
  lacks (as happened with FLAKY-006 before P1-052),
  the checkout will refuse — use the stash dance
  (`git stash push` → `checkout phase-1` → `git stash
  pop`) and verify post-merge that both sides' ISSUES.md
  entries survive.
- **Then:** P1-054 — Overage policy (Type: feat,
  Area: billing/plans, Est: S, Deps: P1-053; spec:
  Reject with clear message + plan matrix link; no
  silent queueing; admin quota-grant bypass audited
  (E1.13). AC: no silent overage).
- **P1-053 design (for the 45th — do NOT re-decide
  without cause):** the lease TTL is 1 h (the
  `worker_heartbeats` staleness horizon, ASSUMPTION-019);
  the pool is Redis-only; `acquire` is one atomic Lua
  script (prune expired, idempotent re-acquire,
  cap check); `renew` cannot resurrect a released or
  expired slot; `recover` is the explicit sweep and
  acquire/held prune lazily; a 0 limit admits nothing
  without touching Redis; a dead Redis fails closed.
  No production caller — Phase 2 intake wires acquire
  at job create and release at completion; the P1-051
  pre-check stays DB-count-based until then.
- **Environment:** services are UP (postgres :5432
  accepting, redis PONG, `just` + cargo on PATH via
  `~/.local/bin` + `~/.cargo/bin`). If a reset hits,
  restore per ASSUMPTION-010 and add an update line.
- **No open S1 issues.** Open: S2 DISK-QUOTA-001, S3
  TESTKIT-MINIO-001, S4 FLAKY-003 (one strike) —
  FLAKY-004 fixed, FLAKY-005 fixed, FLAKY-006 fixed.
  CI #37 results (FLAKY-006 confirmation) come via
  the user's logs zip; the coordinator cannot check
  GitHub (2FA SMS does not reach the user, Gmail not
  connected).
- **Working tree (uncommitted, on `phase-1`):** P1-053 —
  `M crates/core/src/lib.rs`, `M
  crates/core/src/redis/keys.rs`, `?? 
  crates/core/src/slots.rs`, `?? 
  crates/core/src/slots_acquire.lua`, `?? 
  crates/core/src/slots_renew.lua`, `?? 
  crates/core/src/slots_held.lua`, `?? 
  crates/core/tests/slots_live.rs`, `M
  docs/03-ARCHITECTURE/03-api-spec.md`, `M
  docs/03-ARCHITECTURE/04-data-model.md`, `M
  docs/05-QUALITY/01-test-strategy.md`, `M
  docs/07-PHASES/PHASE-1-identity-billing.md`, `M
  docs/09-STATE/ISSUES.md`, `M
  docs/09-STATE/PROGRESS.md`, `??` this night-report
  file. Do not discard any of them.

---

# NIGHT-REPORT-2026-11-15 — 44th coordinator, update (29th VM reset, run still in progress)

- **29th VM reset (~00:48 IST, 19:18 UTC):** the runtime
  restarted mid-P1-053-verify (attempt 1, in
  admin_impersonate_audit, 7 test-result lines, 0
  failures — the kill is proven by the log ending
  mid-binary + uptime 4 min at discovery). No os-intent
  replay was running at first check — then pid 777's
  replay STARTED (its `apt-get update` was already
  running, pid 798). Per ASSUMPTION-010 precedent the
  restore WAITS for the replay — do NOT install in
  parallel. At 19:30 UTC (uptime 12 min) the replay was
  STILL running and `/usr/bin/redis-server` + `psql`
  were still absent. The P1-053 working tree survived
  intact (uncommitted, on `phase-1`); `just` + cargo
  survived.
- **Resume for the 45th if this run ends mid-restore:**
  (1) wait for the replay to finish (check `pgrep -f
  os-intent` + the two binaries); if it restored the
  binaries, skip the apt install, else self-install
  (`sudo apt-get update` + `sudo apt-get install -y
  redis-server postgresql-16`); (2) start services per
  ASSUMPTION-010 (`pg_ctlcluster 16 main start` +
  setsid redis with the documented flags, PONG;
  recreate `pf` role/db via `sudo -u postgres psql`,
  password `pf`, grep-first extraction verified;
  `just migrate`, all 17 migrations); (3) re-run the
  P1-053 full verify FROM SCRATCH (attempt 2, log
  `.scratch/verify-p1-053-attempt2.log` — attempt 1
  cannot be resumed); (4) add the 29th-reset update
  line to ASSUMPTION-010 in the P1-053 commit; (5)
  then commit + merge per the resume state above.
  Expected verify total is still **844 passed, 0
  failed**.

---

# NIGHT-REPORT-2026-11-15 — 44th coordinator, restore update (29th reset, run still in progress)

- **Replay finished, postgres back, redis NOT back:** the
  os-intent replay (pid 777) exited during the wait —
  later `pgrep -f os-intent` hits were false positives
  (the pattern matched the coordinator's own shell).
  `/usr/bin/psql` is back; `/usr/bin/redis-server` is
  absent (`dpkg-query`: no redis-server package known).
  `just` + cargo survived; the P1-053 tree survived
  intact on `phase-1`.
- **apt-get update stalled, killed, retried:** the
  first foreground `apt-get update` wedged ~9 min on
  the noble InRelease/Packages fetch (0 CPU, no log
  writes) — killed the chain (pids 9011/9013/9014/9015,
  confirmed stopped) after clearing
  `/var/lib/apt/lists/partial`. A direct `apt-get
  install -y redis-server` beforehand failed: the
  local lists lack libjemalloc2/liblzf1 (redis-tools
  deps), although the redis-server + redis-tools .debs
  ARE in `/var/cache/apt/archives` (22 .debs, 216 MB).
  The retry update (pid 10160 wrapper, log
  `.scratch/restore-29-apt-update2.log`, then install
  to `.scratch/restore-29-apt-install2.log`) IS
  progressing but the cogentco mirror is flaky — many
  `Ign` lines on noble-updates / noble-security
  InRelease and noble/main Packages. At 19:56 UTC it
  was still running.
- **If this run ends before the restore completes, the
  45th coordinator:** (1) check the two logs above —
  if `INSTALL_EXIT=0` and `/usr/bin/redis-server`
  exists, continue; if the update/install failed,
  retry `sudo apt-get update` then `sudo apt-get
  install -y redis-server` (a third attempt is fine —
  the mirror drops fetches and apt retries); (2)
  start services per ASSUMPTION-010 (`pg_ctlcluster
  16 main start` + setsid redis with the documented
  flags, PONG; recreate `pf` role/db via `sudo -u
  postgres psql`, password `pf`, grep-first
  extraction verified; `just migrate`, all 17
  migrations); (3) re-run the P1-053 full verify FROM
  SCRATCH (attempt 2, log
  `.scratch/verify-p1-053-attempt2.log` — attempt 1
  was killed by the reset, log
  `.scratch/verify-p1-053.log` ends mid-
  admin_impersonate_audit, 7 binaries, 0 failed);
  (4) add the 29th-reset update line to ASSUMPTION-010
  in the P1-053 commit; (5) commit + merge per the
  resume state above. Expected verify total is still
  **844 passed, 0 failed**.

---

# NIGHT-REPORT-2026-11-15 — 44th coordinator, restore outcome (run ending)

- **Redis restored: PONG.** `redis-server` installed
  (UPDATE_EXIT=0 with a noble-security InRelease fetch
  warning, INSTALL_EXIT=0) and started with the
  documented flags (`setsid redis-server
  /etc/redis/redis.conf --daemonize yes --save ""
  --maxmemory 256mb --maxmemory-policy allkeys-lru`);
  `redis-cli ping` = PONG at ~20:00 UTC.
- **Postgres server NOT installed — install running:**
  the replay restored only postgresql-**client**-16
  (`psql` works); the server package is absent — no
  `postgres` user, no `/var/lib/postgresql`, no
  `/etc/postgresql`, no `pg_ctlcluster`. `sudo apt-get
  install -y postgresql-16` launched detached (pid
  11347 wrapper), log
  `.scratch/restore-29-apt-install-pg.log` — the
  postgresql-16 .deb IS in `/var/cache/apt/archives`,
  so the install should complete from cache + the
  refreshed index.
- **45th coordinator, in order:** (1) wait for
  `PG_INSTALL_EXIT=0` in
  `.scratch/restore-29-apt-install-pg.log`, then
  verify `getent passwd postgres` + `pg_ctlcluster`
  exist; (2) `sudo pg_ctlcluster 16 main start`
  (`pg_isready` must answer); (3) recreate `pf`
  role/db via `sudo -u postgres psql` (`CREATE ROLE pf
  LOGIN PASSWORD 'pf'`, `ALTER ROLE pf CREATEDB`,
  `CREATE DATABASE pf OWNER pf` — password `pf`,
  grep-first extraction verified); (4) `just migrate`
  (all 17 migrations); (5) re-run the P1-053 full
  verify FROM SCRATCH (attempt 2, log
  `.scratch/verify-p1-053-attempt2.log`, expected
  **844 passed, 0 failed**); (6) drift checks, fill
  the real numbers into the P1-053 Status line +
  PROGRESS.md health line + P1-053 note, add the
  29th-reset update line to ASSUMPTION-010 in the
  P1-053 commit; (7) commit as one commit
  (`feat(billing): P1-053 Concurrency slots (FR-P-02)`),
  `just lint-commits`, merge `phase-1` into `main`
  (default subject, no rebase, NEVER push); (8) then
  P1-054 — Overage policy.
- **Run-end state:** branch `phase-1`, main at
  `7fd6029` (P1-052 merge). P1-053 tree uncommitted
  and intact (see the resume state above for the file
  list). Redis UP, postgres install IN FLIGHT.
  Nothing is pending push.
