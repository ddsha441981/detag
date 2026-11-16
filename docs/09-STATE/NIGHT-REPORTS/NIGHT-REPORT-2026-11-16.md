# NIGHT-REPORT-2026-11-16 — 46th + 47th + 48th coordinators

## Tickets done

- **P1-053 — Concurrency slots (FR-P-02): DONE + COMMITTED + MERGED.**
  Commit `459599e` (`feat(billing): P1-053 Concurrency slots (FR-P-02)`) on
  `phase-1`, merged to `main` as `73e5b92` (default merge subject, no rebase,
  no push). `just lint-commits` clean. Full `just verify` green: **844 passed,
  0 failed**, attempt 2, ~59 min, no flakes; attempt 1 killed by the 29th VM
  reset (~00:48 IST, mid-admin_impersonate_audit, 7 binaries green, 0 failed)
  — services restored per ASSUMPTION-010 (replay restored psql only; redis
  reinstalled from cached debs after a wedged apt-get update; `pf` role/db
  recreated, all 17 migrations re-applied). Drift checks green (openapi /
  route-table / config-table / env / sqlx-prepare / snapshots). New
  `crates/core/src/slots.rs` (`SlotPool` over Redis sorted set
  `pf:slots:{user}`, 1 h lease — ASSUMPTION-019) + 3 Lua scripts +
  `crates/core/tests/slots_live.rs` (9 live tests). Docs: api-spec,
  data-model, test-strategy, phase file, PROGRESS.md, ASSUMPTION-019, the
  29th-reset update line in ASSUMPTION-010. The 2026-11-15 night report was
  committed in the same commit (it is tracked).
- **P1-054 — Overage policy (FR-P-06): DONE + COMMITTED + MERGED.**
  Implemented by the 46th coordinator, finished by the 47th (adopted the
  uncommitted tree, verified independently, committed as one commit
  `1ca0690` (`feat(billing): P1-054 Overage policy (FR-P-06)`), merged
  to `main` as `6030f7a` (default merge subject, no rebase, no push).
  `just lint-commits` clean. Full `just verify` green: **850 passed,
  0 failed** (844 + 6 new: 4 overage_policy integration + 1 core error unit
  + 1 api quota unit), attempt 2, ~48 min, no flakes; attempt 1 (46th, pid
  12053) killed ~03:37 IST mid-quota_middleware (41 binaries green,
  0 failed) when the 46th coordinator's session was cancelled — NOT a VM
  reset (uptime proves no restart since the 30th reset at ~02:50 IST).
  Drift checks green (openapi / route-table / config-table / env /
  sqlx-prepare / snapshots).

## P1-054 — what was implemented

Every quota denial (PF-3001/3002/3008) now carries `upgrade_url` =
`{PF_PUBLIC_BASE_URL}/plans` in the problem body, plus the FR-P-06 upgrade
hint and the `X-Quota-*` headers. Design (do NOT re-decide without cause):
`Problem.upgrade_url: Option<String>` (`skip_serializing_if None`, so every
other error body is byte-identical — the error-contract golden bodies are
untouched) + `with_upgrade_url` builder in `crates/core/src/error.rs`;
`quota::plans_url(public_base_url)` in `apps/api/src/services/quota.rs`
(trims trailing `/`, appends `/plans`); the `POST /v1/jobs` Denied arm in
`apps/api/src/routes/jobs.rs` attaches it. New
`apps/api/tests/overage_policy.rs` (4 live tests) proves no silent queueing
— after a denial: no `jobs` row, `usage_counters` unchanged (or still
absent), `SlotPool::held` reads 0 — and that the allowed create carries no
link. New `docs/06-DELIVERY/09-overage-policy.md` (denial contract,
no-silent-queueing proof, grant-bypass rule, what overage is not).
ASSUMPTION-020 (link shape `{PF_PUBLIC_BASE_URL}/plans`, console serves
`/plans` in Phase 4; link only on the three quota denials; the only bypass
is an admin quota grant, P1-107 — until then no bypass exists). Docs:
api-spec quota paragraph, test-strategy P1-054 section, quota-ui-contract
denials table (+ `upgrade_url` column), phase file (P1-054 ticked `[x]`, AC
ticked, Status DONE), PROGRESS.md P1-054 note + health snapshot (850) +
Phase 1 count 53 → 54 + current ticket P1-055. OpenAPI regenerated
(`just openapi`, `just openapi-check` green — `upgrade_url` is an optional
string property on Problem, not in `required`). The 30th-reset update line
for ASSUMPTION-010 is added in this commit.

Targeted green (re-verified by the 47th after adopting the tree):
overage_policy 4/4; error_contract 5/5, quota_middleware 9/9,
quota_headers 8/8, core error lib (incl. the new `upgrade_url` unit test),
api quota unit 1/1 (`plans_url` trims slashes), fmt + clippy clean.

## 30th VM reset (~02:50 IST, 21:20 UTC)

The runtime restarted mid-targeted-tests (the neighboring-suites run:
error_contract / quota_middleware / quota_headers — the overage_policy
4/4 and the unit tests had already passed). Uptime 0 min at discovery.
The P1-054 tree survived intact (uncommitted, on `phase-1`); `just` +
cargo survived. The os-intent replay (pid 757) was running — waited it
out per ASSUMPTION-010, did NOT install in parallel; it exited after
~90 s and DID restore the binaries itself (`psql`, `redis-server`,
`redis-cli` back). Restored per the usual steps (`pg_ctlcluster 16 main
start` + setsid redis, PONG; `pf` role/db recreated via
`sudo -u postgres psql`, password `pf`, grep-first extraction verified;
`just migrate`, all 17 migrations applied cleanly). The neighboring
suites were re-run green after the restore (error_contract 5/5,
quota_headers 8/8, quota_middleware 9/9).

## P1-055 — Plan feature flags (FR-P-01): IMPLEMENTED, full verify PENDING

Implemented by the 47th coordinator, uncommitted on `phase-1`
(HEAD `1ca0690` + the tree below). `Entitlements::grants`
in `crates/core/src/plans.rs` (`api_keys`/`webhooks`/`batch`
from the `features` map, `priority_lane` from the `lane` —
a map entry can never grant it) + `require_feature` in
`apps/api/src/services/entitlements.rs` (the same effective
plan the snapshot reads; denies `AppError::FeatureNotEntitled`,
PF-3003, 403, detail carries the FR-P-06 upgrade hint).
Gates: `POST /v1/api-keys` in `services/api_keys.rs`
(validate → gate → cap; a denied create mints no row, writes
no audit row, spends no cap slot; plan-store failure maps to
`Internal` — a down plan store must not mint a key it cannot
gate) + `POST /v1/webhook-endpoints` via the new
`create_webhook_endpoint` handler in `routes/billing.rs`
(Free JWT caller 403, entitled caller 501 stub; a key caller
passes the gate to the stub — `VerifiedApiKey` carries no
owner id — documented in ASSUMPTION-021). New
`apps/api/tests/feature_flags.rs` (7 live tests) +
`grants_covers_all_four_gated_features` unit test + new
`TestApp::grant_plan` harness helper (`common/mod.rs`).
`batch` and `priority_lane` have NO call site yet (no batch
endpoint, job create still the stub) — mapping proven by the
unit tests only; call sites land with the batch endpoint and
the Phase 2 intake. PF-3003 carries the upgrade hint in
`detail` only — `upgrade_url` stays quota-denials-only
(ASSUMPTION-020). ASSUMPTION-021 records the five decisions;
ASSUMPTION-015 clause (1) is closed, (2)/(3) still stand.
Docs: api-spec feature-flags paragraph, test-strategy
P1-055 section, phase file (ticked, Status IMPLEMENTED),
PROGRESS.md P1-055 note, ASSUMPTION-021 + the 31st-reset
update line in ASSUMPTION-010. OpenAPI regenerated (`just
openapi` — the new 403 on `POST /v1/api-keys`, confirmed in
the spec) + `just openapi-check` green.

Targeted green: feature_flags 7/7; core plans 6/6 (incl. the
new grants test); api_keys 18/18, api_key_lifecycle 3/3,
api_key_matrix 4/4, api_key_rate_limits 6/6, key_attribution
5/5, quota_headers 8/8, idor_sweep 3/3, role_escalation 4/4,
admin_guard 3/3, authz_matrix 3/3, extractor 12/12,
route_table 3/3, error_contract 5/5; fmt + clippy clean.
The gate rippled through 10 existing suites — every test
that mints a key now grants its user Pro first
(`grant_plan`); `quota_headers`' key-caller test re-pinned
its expected headers to the Pro limits (500/3600);
`idor_sweep`'s stub-rows test grants its probing user Pro
so the webhook-endpoints row still reaches the 501 stub.

Uncommitted tree (all on `phase-1`, HEAD `1ca0690`):
modified `apps/api/src/routes/api_keys.rs`,
`apps/api/src/routes/billing.rs`,
`apps/api/src/services/api_keys.rs`,
`apps/api/src/services/entitlements.rs`,
`apps/api/tests/admin_guard.rs`,
`apps/api/tests/api_key_lifecycle.rs`,
`apps/api/tests/api_key_matrix.rs`,
`apps/api/tests/api_key_rate_limits.rs`,
`apps/api/tests/api_keys.rs`,
`apps/api/tests/authz_matrix.rs`,
`apps/api/tests/common/mod.rs`,
`apps/api/tests/extractor.rs`,
`apps/api/tests/idor_sweep.rs`,
`apps/api/tests/key_attribution.rs`,
`apps/api/tests/quota_headers.rs`,
`apps/api/tests/role_escalation.rs`,
`crates/core/src/plans.rs`,
`docs/03-ARCHITECTURE/03-api-spec.md`,
`docs/05-QUALITY/01-test-strategy.md`,
`docs/07-PHASES/PHASE-1-identity-billing.md`,
`docs/09-STATE/ISSUES.md`, `docs/09-STATE/PROGRESS.md`,
`docs/api/openapi.json`; untracked
`apps/api/tests/feature_flags.rs`; plus this night report
(edited on `main`, uncommitted — see resume note 1).

## 31st VM reset (~05:20 IST, 23:50 UTC)

The runtime restarted while the 47th ran the new
feature_flags suite (the binary failed at harness spawn:
postgres connection refused). Uptime 5 min at discovery;
no os-intent replay was running. Binaries + data dirs
survived (`psql`, `redis-server`, `/etc/redis`,
`/var/lib/postgresql/16` present) — no reinstall needed.
Restored per the usual steps (`pg_ctlcluster 16 main
start` + setsid redis, PONG; the `pf` role/db were gone
again — fresh cluster — recreated via `sudo -u postgres
psql`, password `pf`, grep-first extraction verified;
`just migrate`, all 17 migrations applied cleanly). The
P1-055 working tree survived intact (uncommitted, on
`phase-1`); `just` + cargo survived. The feature_flags
suite re-ran green 7/7 after the restore.

## P1-055 — Plan feature flags (FR-P-01): DONE + COMMITTED + MERGED (48th coordinator)

Implemented by the 47th, finished by the 48th (adopted the uncommitted
tree, verified independently, committed as one commit
`feat(billing): P1-055 Plan feature flags (FR-P-01)` on `phase-1`,
merged to `main` as a plain merge, default subject, no rebase,
no push — see `git log` for the hashes). `just lint-commits` clean. Full `just verify` green: **858
passed, 0 failed** (850 + 7 new feature_flags integration + 1 new core
`grants()` unit test), attempt 1, ~53 min, no flakes — the exact count
the 47th predicted. Drift checks green (openapi / route-table /
config-table / env / sqlx-prepare / snapshots). The 48th's run started
after the **32nd VM reset (~04:49 IST)** — uptime 49 min at driver
check; postgres/redis were already up at the 48th's first check and
the `pf` role/db + all 17 migrations were verified present before the
verify (no reinstall, no re-migration needed). No new reset hit the
verify itself.

## Resume for the 49th coordinator

1. **Next ticket: P1-056 — Admin plan editor** (Type: feat, Area:
   billing/plans, Est: M, Deps: P1-055; spec: GET/PUT
   /v1/admin/plans (validated entitlements + stripe price
   mapping), cache invalidation, audited; plan removal
   requires no-active-subs check. AC: editor + guard tests).
   Then continue down Phase 1 in order (P1-057 entitlement
   suite, ...).
2. Standing rules: full `just verify` per ticket (slow is
   acceptable); branch-based work on `phase-1`, merge into
   `main` locally; NEVER push/fetch/rebase/set a remote;
   compliance is law; owner decisions to DECISIONS.md tagged
   owner-input-needed with safe defaults.
3. S1 check: 0 open S1 (re-verified by the 48th by script +
   PROGRESS.md health snapshot; the only open items are S2
   DISK-QUOTA-001, S3 TESTKIT-MINIO-001, S4 flakies,
   PERF-001, CI-WORKFLOWS-001, and open ASSUMPTION entries).
   The body's old CI-FAIL-001/002/003 list is STALE — all
   three were fixed long ago; do not re-fix them.
4. The verify log for P1-055 is at
   `~/workspace/goals/detag-watermark-removal-saas/hidden_files/verify-p1-055.log`
   (858 passed, 0 failed, VERIFY_EXIT:0).

## Run-end state (48th coordinator)

- Branch `main`, HEAD = the P1-055 merge (see `git log`).
  `phase-1` HEAD = the P1-055 commit (see `git log`).
- Services UP: postgres accepting connections, redis PONG
  (verified present after the 32nd reset; all 17 migrations
  applied).
- No S1 blockers. No owner decision needed. No verify running;
  nothing uncommitted.

## Risks

- The `upgrade_url` contract points at a console page (`/plans`) that
  does not exist until Phase 4 — documented in ASSUMPTION-020, but the
  URL is untestable live until then.
- A coordinator session being cancelled kills a `setsid nohup` verify
  launched from it (attempt 1 died this way, 41 binaries in) — the
  verify does NOT reliably outlive its launching session. If a verify
  is still running at run end, the next coordinator must check the log
  tail + `ps` first: a log ending mid-binary with no live process means
  re-run from scratch.
