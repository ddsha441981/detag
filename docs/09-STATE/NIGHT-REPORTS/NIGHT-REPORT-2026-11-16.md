# NIGHT-REPORT-2026-11-16 — 46th + 47th coordinators

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
  uncommitted tree, verified independently, committed as one commit — hash
  in the run-end state below). Full `just verify` green: **850 passed,
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

## Resume for the 48th coordinator

1. **P1-055 — Plan feature flags** (Type: feat, Area: billing/plans, Est:
   S, Deps: P1-054; spec: features jsonb (api_keys, webhooks, batch,
   priority_lane) checked by services; 403 PF-3003 with upgrade hint. AC:
   gated features tested). Then continue down Phase 1 in order (P1-056
   admin plan editor, P1-057 entitlement suite, ...). Prep done by the
   47th: `Entitlements.has_feature` already exists in
   `crates/core/src/plans.rs` (features map keeps only boolean values,
   fail-closed); `FeatureNotEntitled` → PF-3003, 403 already exists in
   `crates/core/src/error.rs`; seeded plans already carry
   `api_keys`/`webhooks`/`batch` flags (see plans.rs tests). The ticket
   is the wiring: services check the flag at the gated call site and
   deny PF-3003 with the FR-P-06 upgrade hint. Note: `priority_lane`
   is NOT in the features map — the lane is the separate `Lane` field
   on `Entitlements`; decide how the lane flag is checked (probably via
   `lane == Priority`) and record it as an assumption if the spec is
   silent. Also note ASSUMPTION-015: key creation currently has NO plan
   gate (Free can mint keys) — P1-055's `api_keys` check is where that
   gate would land; the P1-033 tests seed Free users, so they will need
   updating if the gate is added here.
2. Standing rules: full `just verify` per ticket (slow is acceptable —
   P1-053 took ~59 min, P1-054 ~48 min); branch-based work on `phase-1`,
   merge into `main` locally; NEVER push/fetch/rebase/set a remote;
   compliance is law; owner decisions to DECISIONS.md tagged
   owner-input-needed with safe defaults.
3. S1 check: 0 open S1 (re-verified by the 47th by script + PROGRESS.md
   health snapshot; the only open items are S2 DISK-QUOTA-001, S3
   TESTKIT-MINIO-001, S4 flakies, PERF-001, CI-WORKFLOWS-001, and open
   ASSUMPTION entries). The body's old CI-FAIL-001/002/003 list is
   STALE — all three were fixed long ago; do not re-fix them.

## Run-end state (47th coordinator)

- Branch `phase-1`, HEAD `459599e` (P1-053) before the P1-054 commit;
  P1-054 commit hash + merge hash: see the commit log at run end
  (the 47th's final report carries them).
- Local `main` = `73e5b92` (P1-053 merge) before the P1-054 merge.
  Remote `main` was `7fd6029` at the last driver check (before the
  P1-053 merge) — the driver pushes; NOTHING is pending from the
  coordinator side except the P1-053 + P1-054 merges, which the driver
  will push on its next sync.
- Services UP: postgres accepting connections, redis PONG (restored
  after the 30th reset; all 17 migrations applied).
- No S1 blockers. No owner decision needed.

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
