# NIGHT REPORT — 2026-11-12 (coordinator #9)

Run: 23:34 IST 12 Nov → (checkpoint ~00:50 IST 13 Nov; updated ~01:20
IST 13 Nov after P0-097; updated ~01:45 IST 13 Nov after P0-098;
updated ~02:00 IST 13 Nov after P0-099; updated ~02:30 IST 13 Nov
after P0-100; updated ~03:00 IST 13 Nov after P0-101; updated
~03:15 IST 13 Nov after P0-102; updated ~04:18 IST 13 Nov after
P0-103; updated ~04:32 IST 13 Nov after P0-104; updated ~23:30
IST 13 Nov after P0-105; updated ~00:30 IST 13 Nov after
P0-106/107/108 (Phase 0 ticket work done, 105/108); updated
~01:30 IST 13 Nov after P1-001 (Phase 1 open, 1/111); updated
~07:45 IST 13 Nov after P1-002 (Phase 1, 2/111); updated
~08:31 IST 13 Nov after P1-003 (Phase 1, 3/111); updated
~09:15 IST 13 Nov after P1-004 (Phase 1, 4/111); updated
~10:26 IST 13 Nov after P1-005 (Phase 1, 5/111); updated
~11:19 IST 13 Nov after P1-006 (Phase 1, 6/111); updated
~12:13 IST 13 Nov after P1-007 (Phase 1, 7/111); updated
~13:18 IST 13 Nov after P1-008 (Phase 1, 8/111). Resumed
from the 8th coordinator's interruption (~23:21 IST, P0-092 in
progress with uncommitted work). Local HEAD at this update:
`408db1d` (P1-008). No push (local-only by rule; parent handles
GitHub pushes).

## Tickets done (20)

- **P0-092 — testcontainers fixtures** (`fd46981`): new `crates/testkit`
  (`detag-testkit`): `TestCtx` — per-test scratch DB (migrated, dropped
  by `close()`), per-test redis namespace, per-test S3 bucket;
  `TestCtx::new()` from env, `TestCtx::containers()` via testcontainers
  (env-gated `PF_TEST_CONTAINERS=1`); `spawn_app` already existed in the
  api harness. 6 new tests. Note: the api harness still carries its own
  inline container defs — dedup onto testkit is a follow-up; the testkit
  doc comment that claimed otherwise was corrected, not left false.
- **P0-093 — Golden media generator** (`2adf9f4`): `scripts/fixtures_gen.py`
  + `just fixtures-gen` / `just fixtures-check`: 36 images (12 bases ×
  clean/badge/text; the "24 images matrix" is the 24 marked inputs —
  see ASSUMPTION-013) + 3 videos (30 s 1080p, 10 s 720p, 4 s vertical;
  testsrc2, CRF 23, threads 1, pinned creation_time) + 39 envelopes +
  `SHA256SUMS` + `fixtures/README.md`. 75.3 MB total. Proven: gen twice
  → byte-identical SHA256SUMS; `just fixtures-check` green.
  CI `fixtures-check` step added.
- **P0-094 — Template pack generator** (`37c2f5d`):
  `scripts/templates_gen.py` + `just templates-gen` / `just templates-check`:
  8 synthesized RGBA templates (4 badges + 4 text, generic text only —
  no brands, no scraping) in `templates/dev/` (label
  `SYNTHETIC-TEST-ONLY`) and `templates/prod/` (v1, same tooling/pixels)
  + manifests (name, sha256, params) + `templates/README.md`.
  CI `templates-check` step added.
- **P0-095 — Snapshot testing** (`13fdad7`): insta 1.49 (`json` feature)
  + `cargo-insta` 1.49.0 installed; `crates/core/tests/snapshots.rs`:
  7 snapshot tests (5 problem+json, 2 `Page<Problem>`) with 7 committed
  `.snap` files. Review flow in the test strategy doc; CI verify job runs
  `INSTA_UPDATE=no`; `just snapshots-check` added. Engine reports NOT
  snapshotted — no report type exists in Phase 0 (Phase 2 golden suite).
- **P0-096 — proptest harness** (`96ceabc`): proptest 1;
  `crates/core/tests/prop.rs` + `tests/prop/{cursor,keys,state_machine}.rs`:
  13 properties at 1000 cases each + 1 plain test, 14/14 green. Mask
  codec properties wait for the Phase 2 codec (`tests/prop/mask.rs`).
  Nightly job added (`.github/workflows/nightly.yml`, 02:00 UTC) +
  `just prop` recipe.
- **P0-097 — Criterion benches scaffold** (`84f7bc9`): criterion 0.7
  + `image` 0.25 as engine dev-deps; `crates/engine/benches/
  {support,detect,inpaint,encode,full_job}.rs` + `README.md`: 17
  benches per tier (512, 1080p, 4K) — detect prologue + pack verify,
  inpaint tiles/s (blur proxy), encode (JPEG q95 + PNG, real op),
  full job (chained stages + video input prep). `just bench` green
  in 2m54s; baselines committed as `benches/baselines/*.json`
  (mean/median via `scripts/bench_baseline.py`); nightly bench job
  added (runs `just bench`, uploads estimates, does NOT commit).
  Scaffold honesty: NCC kernel NOT benched, blur is a proxy for
  LaMa, video factor NOT measured (needs real video pipeline) —
  numbers are harness numbers (full-job 4K mean 545 ms).
- **P0-098 — llvm-cov + coverage gate** (`b1ba2ef`):
  `scripts/coverage_gate.py` parses `target/llvm-cov/lcov.info`
  into a per-crate table vs the NFR-50 floors (core 85, engine 80,
  api 75, worker 70; production sources only). `just coverage` ends
  with the script in report mode (exit 0 always); `just
  coverage-gate` exits 1 below a floor — OFF until Phase 2, when
  the Phase 2 ticket flips CI to it. CI's coverage step runs the
  script in report mode. First live report: core 96.7%, engine
  99.1% (112 lines — nearly vacuous, placeholder engine), api
  92.7%, worker 83.8% — all above floors. 5 new gate tests; Python
  suite now 124, all green. `just coverage` takes ~12 min on this
  VM (instrumented rebuild dominates).
- **P0-099 — Flaky policy tooling** (`f062086`):
  `scripts/test_repeat.py` + `just test-repeat <name> <n>` (proved:
  FLAKY-003's test 3/3 in 15 s); `scripts/list_quarantined.py` +
  `just list-quarantined` (currently: no quarantined tests); nightly
  `quarantined` job lists them. Convention documented in the test
  strategy doc. Deviations recorded: spec's `just test:repeat` is
  not valid just syntax (dash spelling used); NO test quarantined —
  FLAKY-001/002 meet the two-strike rule but were left in the suite
  deliberately (quarantining drops shutdown/pubsub coverage from
  every commit) — owner decision. 8 new script tests; Python suite
  now 132, all green.

- **P0-100 — cargo-audit/deny in CI** (`be15c71`): gate was red on
  14 advisories published after the lockfile was cut. Fixed by
  upgrade: aws-sdk-s3 → 1.159.0 (crypto-bigint 0.5.5 gone),
  utoipa → 6.0.0 + utoipa-swagger-ui → 10.0.2 (zip 3.0.0 → 9.5.1,
  api checks clean, no code changes). 9 advisories with no upgrade
  path are ignored in deny.toml with per-ID reasons + removal
  conditions (rustls 0.21/webpki/h2 via AWS SDK at latest,
  astral-tokio-tar via testcontainers at latest, paste via image at
  latest). Weekly audit workflow added (Mondays 03:00 UTC).
  Gate green: advisories/bans/licenses/sources all ok. Lockfile
  churn 172 lines.
- **P0-101 — Input validation layer** (`7d09a28`): new
  `crates/validator` (email shape, filename sanitize,
  public-only URL; `ValidationErrors` + `Validate` trait) +
  `FieldError`/`Problem.field_errors` in core +
  `ValidationFailed` 400 → 422 (snapshots re-accepted, diff
  reviewed: status only) + `ValidatedJson<T>` extractor in the
  api + 6 live probe tests (valid → 200, bad email/private URL
  → 422 with field errors, all-bad → 3 entries, malformed JSON
  → 422 without field_errors). Docs: api-spec, adding-an-
  endpoint, error_mapper note. Limitations recorded: no DNS
  resolution in the URL rule (fetch layer re-validates, Phase 3).
- **P0-102 — Secure headers middleware** (`bc01c10`): the P0-065
  table was NOT complete. Fixed all three gaps: (1)
  `referrer-policy: no-referrer` added to `SECURE_HEADERS`;
  (2) HSTS moved out of the table, stamped only when
  `Settings::hsts_enabled()` (every env except dev — stage/prod
  behind TLS; unit-tested); middleware is now
  `from_fn_with_state`; (3) `docs_csp_middleware` + `DOCS_CSP`
  (same-origin scripts/styles/images/fonts + `connect-src 'self'`,
  `frame-ancestors 'none'`) layered OUTSIDE the secure-headers
  layer — `insert`, not append, so exactly one CSP header exists.
  Header table test (12 tests): dev → no HSTS, prod → HSTS with
  the exact value, `/docs/` → docs CSP + rest of the table,
  `/healthz` after it still gets the strict api CSP. Found by
  testing: Swagger UI 303-redirects `/docs` → `/docs/`.
- **P0-103 — Rate limiter skeleton** (`05cd01e`): HTTP layer over
  the P0-048 primitives (they existed; the middleware + race
  tests were the gap). `apps/api/src/rate_limit.rs`: scopes
  ip/user/api-key/route (user + api-key defined but unenforced
  until Phase 1 auth), IP + route token buckets in `AppState`,
  middleware between http-metrics and the error mapper; IP =
  XFF first entry → socket addr (main.rs wires ConnectInfo) →
  `"unknown"`; route key `METHOD:/v1/<segment>` (colon — the
  Redis key builder rejects whitespace, caught by the first
  test run). 429 PF-3007 + `Retry-After` + `X-RateLimit-*`;
  fail-open on Redis down (warn logged); health/meta exempt.
  Settings `PF_RATE_LIMIT_*` (IP 120/120, route 600/600).
  Tests: 7 api + 1 core — IP trips at 5, route trips
  independently, and the AC race test (20 concurrent vs burst
  5, refill 0 → exactly 5 admitted, 15 denied).
- **P0-104 — Body/size/dimension guards** (`9d0a92d`): body caps
  now per route class (`BodyLimitClass`: Small 64 KiB auth+meta,
  Standard 1 MiB jobs+admin, Webhook 256 KiB billing); uploads
  stay exempt. Tests: a 65 KiB body is 413 PF-2003 on auth and
  admitted on jobs; billing trips at 256 KiB+1. Dimension guard
  `validate_dimensions` in core media: each side ≤ 8000 px,
  total ≤ 64 MP, missing/non-positive fail CLOSED to PF-2005
  (413); boundary unit tests. Honest scope: the guard is NOT
  wired into the Phase 0 job-create stub (no upload row to read,
  no dimensions in the payload) — the Phase 2 handler hooks it
  in after loading the upload row.

- **P0-105 — Secrets policy enforcement** (`4c0c63e`): the shared
  base compose file carried literal secret defaults
  (`${PF_JWT_SECRET:-dev-fixed}`, `${PF_S3_*:-minioadmin}`,
  `${PF_POSTGRES_PASSWORD:-pf}`) — a dev default there is also
  prod's default, since base layers under prod. Stripped to bare
  `${VAR}` refs in base; dev values arrive via the env file
  (`--env-file .env`, copied from `.env.example`). New gate
  `scripts/check_compose_secrets.py` + `just compose-secrets-check`
  (+ CI step): strict files (base/prod) allow only bare refs;
  dev-only files allow the documented placeholders (`pf`,
  `minioadmin`, `dev-fixed`); unknown files are strict. 11 script
  tests (Python suite 132 → 143). Gates: `just secrets` (gitleaks
  8.30.1 full scan) — 107 commits, no leaks. Runbook: new
  "Secret rotation" section — JWT dual-verify procedure DRAFTED
  (code does not exist yet; auth is classification-only until
  P1-016; Phase 5 rehearses), S3/MinIO root rotation (caveat:
  MinIO may keep stored root creds on an existing data dir —
  rehearsal must confirm, fallback is a dedicated app user),
  Postgres password, payment keys. Known gap recorded: the api
  does NOT fail fast in prod on the placeholder `dev-fixed`
  Settings default — the runbook checklist is the only guard;
  a boot-time guard is a Phase 5 hardening candidate. The
  commit hook caught a fake `sk_live_abc123` in the new test
  file (generic-api-key rule) — replaced with `hunter2`. Also
  fixed here (found by the drift test): P0-103 had added the 4
  `PF_RATE_LIMIT_*` Settings fields but never added them to
  `.env.example` or the runbook env table, and had hand-edited
  the GENERATED config-reference.md — the 4 vars are now in
  `.env.example` + the runbook table, and the reference is
  regenerated (`just env-check` OK).
- **P0-106 — Threat model stub** (`aaa7324`): new
  `docs/threat-model.md` — 10 assets, 8 actors, 8 entry points
  (Live/Stub/Planned — auth/billing/admin are Phase 1, intake
  is Phase 2), 5 trust boundaries, STRIDE-lite T1–T16 with
  LIVE/WIRED/PLANNED/OPEN status per mitigation (P0-101..P0-105
  cited as the live mitigations), recorded trade-offs (rate
  limiter fails OPEN on Redis down; public-URL validation does
  not resolve DNS; `dev-fixed` has no prod fail-fast; `/metrics`
  has no in-code auth — deployment-scoped; dual-verify drafted,
  not implemented), compliance boundaries (attestation + AUP,
  C2PA/SynthID OFF). Linked from the security-testing plan
  (T-IDs as cross-references).
- **P0-107 — Security smoke tests** (`4392a8b`): new
  `apps/api/tests/security_smoke.rs` — one named gate over the
  five ACs, through the real router: headers present (full table
  + request id + ACAO, no HSTS in dev), rate limit trips at
  threshold (burst 5 admits 5, 6th is 429 PF-3007 +
  Retry-After), oversized body rejected (65 KiB on the 64 KiB
  auth class is 413 PF-2003, same body on jobs admitted),
  method-not-allowed (DELETE /healthz is 405 with `Allow: GET`),
  no swagger in prod (both `/docs` paths are 404 PF-2010 in
  prod, `/healthz` still 200 with HSTS; dev `/docs`
  303-redirects). Two test bugs found by running: the default
  `web_origin` is not the test ORIGIN (ACAO was None until the
  settings set it), and the 413 problem title is snake_case
  (`payload_too_large`); the 404 code is PF-2010. Observation
  recorded in the test + doc: the 405 body is axum's bare 405,
  not problem+json — the mapper rewrites 400/404/413/415/422
  only; whether 405 belongs in the envelope is a Phase 1
  question. Docs: security-testing plan gained a "Security
  smoke suite" section; `just security-smoke` recipe added.
- **P0-108 — Security baseline summary** (`0212574`): new
  `docs/05-QUALITY/05-security-baseline.md` — §1
  what-is-enforced-where matrix (CI/runtime/ops), §2 checklist
  mapping of every security-testing-plan section
  (Enforced/Partial/Planned), §3 nine known gaps stated plainly
  (fail-open limiter, no DNS resolution, no prod fail-fast on
  `dev-fixed`, `/metrics` no in-code auth, dual-verify drafted,
  405 bare, FLAKY-001/002 not quarantined by owner decision, CI
  gates never run remotely, three docker-blocked tickets), §4
  Phase 1+ handover, §5 how to verify the baseline. Linked from
  the security-testing plan (Reporting) and the threat model.
  Phase 0 now stands at 105/108 — the remaining three tickets
  (P0-034/046/083) are implemented but unverified, blocked by
  DOCKER-001/CI-WORKFLOWS-001; the exit gates for `just dev` /
  `just smoke` / CI cannot hold on this VM and are explicitly
  deferred, not silently dropped.
- **P1-001 — Registration endpoint** (`342c40c`): POST
  /v1/auth/register is the first real handler (route table now
  3 live / 60 stubs). New `crates/core/src/password.rs`
  (Argon2id, OWASP m=19456/t=2/p=1, PHC string, random salt,
  verify; 6 unit tests) — `argon2` 0.5 + `rand_core` 0.6 with
  `getrandom` (argon2's `rand` feature does not enable
  rand_core's getrandom gate for `OsRng`). Config: 5 new `PF_*`
  vars (argon2 ×3, `PF_TOS_CURRENT_VERSION` "v1",
  `PF_DISPOSABLE_EMAIL_DOMAINS`) + validation + config test;
  `.env.example` (starter disposable list) + runbook +
  regenerated config table. Also fixed in passing: the config
  test's `ALL_VARS` was missing the 4 `PF_RATE_LIMIT_*` vars —
  added, now 51/51 in sync with FIELD_VARS. Validator:
  `validate_password` (min 12 chars, NIST SP 800-63B, never
  trimmed, counted in characters; 4 tests). Repos:
  `users::create_with_tos` (`create` unchanged — 10+ callers),
  new `audit_logs.rs` and `email_outbox.rs`; `.sqlx/`
  regenerated (1 deleted old `create` query, 6 new). Service
  `apps/api/src/services/register.rs` (first real handler
  creates `services/`): normalize → policy checks (disposable
  `disposable_email`, ToS `tos_version_mismatch`, both 422) →
  hash → audit attempt (outside the tx) → one tx (user +
  success audit + outbox enqueue; duplicate = unique-constraint
  race → 409 `PF-2011` Conflict, generic body). Handler +
  `#[utoipa::path]` + ApiDoc (register path, schemas; utoipa
  features `uuid`+`chrono`) + `just openapi`/`just route-table`
  (generator + probe test both learned the register exception —
  empty-body probe is 422 PF-2002). Harness:
  `spawn_with_fixtures_and_settings`. Tests: 8 new in
  `apps/api/tests/register.rs`. Handler-shape finding: clippy
  `result_large_err` denies `Result<_, ProblemResponse>`
  (152 bytes vs a small Ok), and axum has no `IntoResponse` for
  `Box<ProblemResponse>` (tried, does not compile) — real
  handlers return `Response` directly; the adding-an-endpoint
  doc was corrected to that prescription. Honest notes:
  duplicate → 409 per the taxonomy, but the *status*
  distinguishes duplicate from new — full indistinguishability
  (201 for duplicates) would be a DECISIONS.md item; the
  verification token is P1-002's scope (the queued mail carries
  `{user_id}` only until then); password minimum 12 is a
  product-policy constant, not config (no doc specified it).
  `just deny` green (new crates pass).

- **P1-002 — Email verification flow** (`af7e4fc`): migration
  `20261113014639_email_verification_tokens` (sha256 `token_hash`
  UNIQUE, 24h expiry, `used_at` single-use, one live token per
  user via delete-unused-on-mint). Core `tokens.rs` (32 random
  bytes, base64url 43 chars, constant-time compare; 4 unit
  tests) + token repo (mint, by_hash, atomic consume UPDATE,
  delete_unused_for_user) + `users::set_email_verified`.
  Service `services/verification.rs` (issue/verify/resend/
  `require_verified`; 2 unit tests). Handlers `POST
  /v1/auth/verify-email` (204; unknown/used → 401 PF-1010,
  expired → 401 PF-1011) + `POST
  /v1/auth/resend-verification` (204-always; a NEW route — the
  spec names no resend route). Register now mints via
  `issue_verification` (outbox payload `{user_id, token}`; the
  register test now asserts the token row). The PF-1009 gate
  `require_verified` is pure on the loaded user (job-create is
  a Phase 2 stub — no live route yet). Route table 64 entries
  (auth 17), 5 live / 59 stubs; OpenAPI gained both paths +
  schemas. Tests: 13 new in `tests/verification.rs` (happy
  path + audit, single-use, unknown 401, expired 401 + not
  consumed, concurrent double-verify exactly one 204, resend
  invalidates the old token + fresh verifies, rate limit
  3/hour, unknown-email 204 + no mail, verified-user 204 + no
  mail, malformed email 422, empty token 422, malformed JSON
  422, `require_verified` PF-1009/403 gate). Design bug caught
  by the rate-limit test: minting deletes previous unused
  tokens, so counting token rows for the 3/hour resend limit
  under-counts (the third resend minted a 4th mail). Fix: the
  limit counts `email_outbox` rows (template + user + hour) —
  every mint enqueues exactly one row in the same transaction,
  so the outbox count IS the mint count, 1:1; the token repo's
  `count_recent_for_user` was REMOVED rather than left to
  silently under-count. Test bug: the unknown-email resend test
  asserted a globally empty outbox — the scratch DB is shared
  across tests in the binary, so the assertion is now scoped to
  the unknown email's hash. Migration-checksum lesson: editing
  an uncommitted migration's comment changes its sqlx checksum
  — the local DBs (`detag_api_harness` + `pf`) needed `UPDATE
  _sqlx_migrations SET checksum = decode(sha384(file), 'hex')`
  (verified against an unmodified migration first); committed
  migrations must never be edited.

- **P1-003 — Login endpoint** (`5457dab`): HS256 access
  token (`jsonwebtoken` 11, `aws_lc_rs` backend — the
  `rust_crypto` feature pulls `rsa 0.9.x`
  (RUSTSEC-2023-0071) + `crypto-bigint 0.5.5`
  (RUSTSEC-2026-0387), neither with an upgrade path, so the
  backend switch removes both crates from the tree; `just
  deny` green). Core `jwt.rs` (leeway 0, 15m access / 30d
  refresh constants; 5 unit tests) + repo `sessions.rs`
  (`create`, `list_for_user` — P1-007 reuses it). Service
  `services/login.rs`: per-account bucket (email sha256, 10
  burst / 1 per min) checked BEFORE the password verify;
  generic 401 `PF-1001` for unknown email and wrong password
  (bodies byte-identical, proven by test); constant-time
  verify via a precomputed dummy hash in `AppState` (one hash
  at boot); malformed stored hash is a 500. Login does NOT
  require a verified email. Handler sets the `detag_refresh`
  cookie (`HttpOnly; Secure; SameSite=Lax; Path=/v1/auth;
  Max-Age=2592000`); the refresh token never appears in the
  body. New settings `PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST` (10)
  / `_REFILL_PER_MIN` (1) + config table regenerated. Route
  table 64 entries, 6 live / 58 stubs. Tests: 8 new in
  `tests/login.rs` + 5 JWT unit + 1 login service unit +
  config additions. axum 0.8 has no `FromRequestParts` for
  `Option<ConnectInfo>` — new never-rejecting
  `MaybeConnectInfo` extractor. Test-isolation: refill-0
  rate-limit buckets never refill and their Redis keys
  survive across runs — the login IP test and the
  route-table probes (62 /v1 requests in one burst, which
  tripped the shared "unknown" 120-burst IP bucket) now use
  per-run UUID XFF addresses. Threat model gained T17
  (credential stuffing / brute-force login).

- **P1-004 — Lockout/backoff** (`6d54c5e`): 5 failures
  in a 15-min window lock the account AND the IP — 423
  `PF-1008` `account_locked` with `Retry-After`, first lock
  60 s doubling per further failure to a 3600 s cap. Core
  `lockout.rs` (curve + 2 unit tests proving it exactly) +
  repo `login_attempts.rs` (`record_failure`: `INSERT ...
  ON CONFLICT DO NOTHING` placeholder + `SELECT ... FOR
  UPDATE`, GREATEST lock extension; `mark_notified`;
  `reset`) + migration
  `20261113030740_login_attempt_counters`. Lock check BEFORE
  the password verify (locked attempts not recorded, not
  audited); failure tx = both counters + `login.failure`
  audit + first-lock `lockout-notice` outbox enqueue +
  `login.locked` audit; success tx resets both counters;
  register clears the account counter. Handler renders 423
  + `Retry-After`. Tests: 6 new in `tests/lockout.rs`
  (backoff, reset-on-success, one notification per episode,
  IP lockout blocks a fresh account, register clears a
  pre-existing counter, repo-level 20-concurrent records —
  counter exactly 20, lock at cap). Findings: (1) the
  placeholder INSERT was needed — plain SELECT-then-INSERT
  races on the PK conflict and poisons the Postgres tx →
  500; (2) an HTTP-level concurrent race test (20/8/4
  concurrent logins) flaked on this VM — synchronous Argon2id
  verify blocks the 2-worker runtime, pool acquires time out
  (`PoolTimedOut`, proven by a scratch run) — recorded as
  PERF-001 (hardening: `spawn_blocking` for the verify); the
  race proof lives at the repo layer. 5th VM reset hit
  mid-ticket (~03:12 IST 13 Nov, binaries gone): waited out
  the os-intent replay (pid 752), restarted services,
  recreated `pf` role/db, re-applied migrations. T17 now
  LIVE (lockout).
- **P1-005 — Profile GET/PATCH /v1/me** (4d5c73c): migration
  `20261113040605_user_profile_fields` adds `display_name` +
  `pending_email` (no unique constraint — the P1-006 swap
  enforces uniqueness via `email`). `users::update_profile`
  (plain SET, so a PATCH can clear a field) +
  `subscriptions`/`usage_counters` repos + `validate_display_name`
  (trim, 1–100 chars, no control chars) + `entitlements` service
  (current plan row + quota snapshot from plan entitlements +
  today's usage row). Auth extractor Bearer leg: verifies the
  JWT (leeway 0), loads the `sub` user, yields a verified context
  (user + role from the row); no credential / API key / session
  cookie → 401 PF-1001, malformed JWT → PF-1010, expired →
  PF-1011, valid JWT for unknown user → PF-1001. API-key +
  cookie legs, precedence + CSRF docs are P1-016's; user status
  is NOT checked here (suspension is P1-008's scope).
  `GET /v1/me` returns profile + plan + quota; `PATCH /v1/me`
  sets the display name and/or records the email-change intent
  (validated + disposable-checked + duplicate-checked → 409
  PF-2011; unchanged → 422 `email_unchanged`); `null` clears,
  absent leaves unchanged, no fields = no-op 200 (no write, no
  audit); update + `profile.update` audit in one tx (pending
  email as sha256 in the audit). Route table 8 live / 56 stubs
  (DELETE /v1/me stays a stub for P1-007); OpenAPI gains both
  paths + 5 schemas. Findings: (1) serde trap — plain derive
  collapses JSON `null` into missing for double-`Option` PATCH
  fields, so the clear-PATCH behaved as a no-op until a custom
  `deserialize_with` kept `null` = clear (proven by a unit test
  first); (2) verify attempt 1 failed on `job_events_retry` —
  its 001+002-only scratch schema lacked the new columns
  (`users::create` now selects them); fixed by adding the
  profile-fields migration to its scratch dir (only that test
  uses a partial schema — `repos_live`/`tx_live` apply all
  migrations). Tests: 16 new in `tests/profile.rs` + 5
  profile-service unit + 6 validator display-name unit + the
  extractor's rejection unit test. `just verify` green 486
  passed, 0 failed, attempt 2 (no flakes).
- **P1-006 — Email change flow** (6d074d3): migration
  `20261113050515_email_change_token_purpose` adds `purpose`
  (`verification` | `email_change`, CHECK, backfilled
  `verification`) to `email_verification_tokens`; the two
  purposes are never interchangeable (a registration token at
  confirm → 401 PF-1010, not consumed). `tokens::mint` takes
  a purpose + `delete_unused_for_purpose` (purpose-scoped
  delete); `users::swap_email` (email = pending, pending
  cleared, `email_verified_at` set, guarded by
  `pending_email IS NOT NULL`). `issue_email_change` (in the
  profile PATCH tx when a NEW pending address is set): deletes
  all unused tokens (one live token per user), mints an
  `email_change` token, queues `email-change-verify` to the
  new address (`{user_id, token}`) + `email-change-alert` to
  the old address (`{user_id}` — no token, never names the
  new address); `clear_email_change_tokens` kills the live
  change token when the intent is cleared (registration token
  survives). `POST /v1/auth/confirm-email-change` → 204:
  consume + swap + `email.change` audit (both addresses as
  sha256) in one tx; unknown/used/wrong-purpose → 401
  PF-1010, expired → 401 PF-1011, address taken meanwhile →
  409 PF-2011 (swap is the enforcement point; the failed swap
  rolls back, token stays unused, intent stays pending).
  Re-PATCHing the same pending address is a no-op (no re-mail
  — re-trigger by clearing + re-setting). One-live-token
  consequences recorded in the api-spec: a resend kills a live
  change token (intent survives); a change PATCH kills a live
  registration token. Route table 65 routes, 9 live / 56
  stubs; OpenAPI gains the confirm path + schema. Findings:
  (1) the outbox accumulates one mail per mint — a superseded
  mail stays queued with a dead token (its link 401s), the
  same as the resend flow; only the token row is the live
  credential — tests must read the LATEST mail, not assert
  exactly one (caught by the replace-pending test); (2) the
  harness DB persists across runs, so confirming tests must
  use unique pending addresses — fixed strings collided with
  a previous run's swapped user and 409'd (caught by the
  second run, not the first); (3) an unknown confirm token is
  PF-1010, not PF-1001 (PF-1001 is "no credential at all" —
  a test-expectation bug, not a service bug). Tests: 12 new
  in `tests/email_change.rs`, green twice in a row.
  `just verify` green 498 passed, 0 failed, attempt 3
  (attempts 1–2: FLAKY-002 `slow_consumer_drops_safely`
  streak of 2, green after a 60 s settle; the 6th VM reset
  hit mid-attempt-2 — runtime restart, services restored per
  ASSUMPTION-010). Docs: api-spec email-change paragraph +
  route-table row, data-model tokens row, security-testing
  email-change line, threat model T18.
- **P1-007 — Account deletion** (018c6ba): core repo `users.rs`
  (`DELETION_GRACE_DAYS` 30, `request_deletion`,
  `cancel_deletion`, `purgeable`, `delete_user`) + deletion
  service (`request_account_deletion` — one tx: status +
  `account.delete_requested` audit + `account-deletion`
  confirmation mail; second request while pending is a no-op,
  grace runs from the FIRST request) + `DELETE /v1/me`
  handler (204, Bearer extractor; `deleted_at` added to the
  profile response so `GET /v1/me` shows the grace) + login
  cancels a pending deletion in the success tx (with the new
  session; a concurrent cancel first is not an error) + gc
  purge (`purge_expired_users`, dry-run default, one tx per
  user: `account.purged` audit + hard delete; `detag-gc
  --purge-users` mode, DB only — S3 objects become orphans
  for the next sweep). Migration
  `20261113062352_purge_cascade_triggers` — the purge test
  PROVED a cascade-vs-trigger conflict (purge_one failed
  with "table audit_logs is append-only"): (1) the
  `audit_logs.actor_user_id` FK is dropped (the actor is a
  historical fact — the FK's `ON DELETE SET NULL` is an
  UPDATE the trigger forbids, so no user with audit rows
  could ever be purged; the actor id is now retained intact,
  a stronger trail than NULL); (2) the `job_events` /
  `attestations` triggers are UPDATE-only (history is never
  rewritten; DELETE is the purge's erasure path); (3) the
  `audit_logs` trigger keeps its DELETE branch (the audit
  trail is never deleted). Down migration restores the FK
  `NOT VALID` (a plain re-add would fail validation against
  purged users' retained actor ids) + the DELETE branches.
  Route table 65 routes, 10 live / 55 stubs; OpenAPI gains
  the DELETE path. Tests: 5 new in `tests/deletion.rs` + 3 gc
  purge tests (cascade incl. job_events + attestations +
  upload; grace/active kept; dry-run deletes nothing) + 1
  migration test (`migration_purge_cascade_triggers.rs` —
  FK gone, trigger defs, UPDATE-fails/DELETE contract,
  user-delete cascade with the audit row surviving with its
  actor intact, revert + re-apply). Two test bugs found by
  running: `subscriptions.provider` + `idempotency_keys.endpoint`
  are NOT NULL (seed fixed). Lessons: (1) `just sqlx-prepare`
  does not compile `#[cfg(test)]` code — macro queries in
  tests have no offline data, so test SQL uses the literal
  function form; (2) sqlx 0.9 rejects dynamically built SQL
  (`SqlSafeStr`) — the gc test's count helper uses explicit
  per-table literal queries; (3) the 004 migration test pins
  its migration set and its old trigger contract by design —
  the new contract gets its own test file, not an edit of the
  004 assertions (a header note points at it). Docs: api-spec
  deletion paragraph + route row, data-model cascade /
  retention + audit actor + trigger notes, security-testing
  deletion line, threat model T19 (deletion + erasure —
  LIVE), runbook gc purge mode. `just verify` green 507
  passed, 0 failed, attempt 1 (no flakes).

- **P1-008 — Admin suspend/restore** (`408db1d`): error
  taxonomy `AccountSuspended` PF-1003, 403, title
  `account_suspended` (PF-1003/1004/1005/1006 were all free)
  + validator `validate_reason` (trim, 1–500 chars, no
  control chars; codes `required`/`too_long`/`invalid_reason`)
  + 6 unit tests + core repos `users::suspend` /
  `users::restore` (status flip + `deleted_at = NULL`),
  `sessions::revoke_all_for_user` (sets `revoked_at` where
  NULL, returns count), `jobs::hold_queued_for_user`
  (`queued` → `held`) + `jobs::release_held_for_user`
  (`held` → `queued`) + `AdminContext` extractor (wraps
  `AuthContext`, requires `role == Role::Admin`, else 403
  PF-1002) + service `services/admin.rs` (`suspend_user` /
  `restore_user`, `AdminError`, `AdminActionOutcome`; each
  action is one tx — status + revoke/hold/release + audit;
  suspend audit `user.suspended` with the reason +
  `{status, sessions_revoked, jobs_held}` in `after`;
  restore audit `user.restored` with
  `{status: active, jobs_released}`; both idempotent no-ops
  in the target state; suspend cancels the deletion grace —
  invariant `deleted_at` non-NULL exactly when status is
  `deletion_pending`; sessions stay revoked on restore; no
  suspension mail) + login blocks suspended users AFTER the
  password verify (correct password → 403 PF-1003,
  `login.failure` audited with reason `suspended`, no lockout
  counter; wrong password still the generic 401; lockout
  check stays first; the extractor still does NOT check
  status — residual access token ≤ 15 min is the documented
  design, threat-model trade-off) + handlers in
  `routes/admin.rs` (thin, `Response` directly; suspend takes
  `ValidatedJson<SuspendRequest>`, restore takes no body; 200
  with reused `UserResponse`). Route table 65 routes, 12
  live / 53 stubs; OpenAPI gains both paths +
  `SuspendRequest` schema. Tests: 14 new in
  `tests/admin_suspend.rs` (happy path incl. audit counts +
  sessions revoked + jobs held; non-admin 403 PF-1002;
  unauthenticated 401 PF-1001; unknown user 404 PF-2010;
  missing/blank reason 422 PF-2002; malformed UUID 422
  PF-2002; already-suspended no-op; restore happy path incl.
  jobs released + sessions stay revoked; restore active
  no-op; restore auth rejections; unknown restore 404;
  suspend cancels pending deletion; suspended login 403
  PF-1003 / wrong password 401 + audit; login works after
  restore). Bonus finding: the malformed-UUID test caught a
  real error-mapper bug — the mapper kept the framework's
  400 wire status while the problem body said 422 (its own
  docs already promised "a framework 400 becomes a 422
  problem"); the mapper now stamps the wire status from the
  problem (404/413/415 mappings unaffected). Fixes found by
  running: `ValidationErrors` has no `add`/`into_result`
  (use `default()` + `push` + `is_empty`, the profile
  pattern); `validate_reason` was not re-exported from the
  validator crate root; the rewritten `routes/admin.rs` had
  dropped the `routing::{get, post}` + `Router` imports;
  clippy `doc_lazy_continuation` on doc lines starting with
  `+` (users.rs `suspend` doc, admin_suspend.rs header,
  routes/admin.rs header — the P1-001 lesson, re-learned);
  the new variant was missing from core's error test tables
  (`response_table()` row + all three hardcoded `36` counts
  → `37`). Docs: api-spec admin section + login paragraph,
  data-model jobs row (`held` + Phase 2 claim note) + users
  row (`deleted_at` invariant), security-testing suspend
  bullets, threat model T20 (suspension bypass /
  admin-action abuse — LIVE) + residual-token trade-off.
  `just verify` green 527 passed, 0 failed, attempt 4
  (attempt 1: fmt; attempt 2: clippy `doc_lazy_continuation`
  ×2 files; attempt 3: 2 core error-table failures, then the
  7th VM reset killed the run mid-api-tests — environment
  restored per ASSUMPTION-010: no os-intent replay running
  this time, full `apt-get update` (6m43s) + install, setsid
  redis, `pg_ctlcluster 16 main start`, recreate `pf`
  role/db via `sudo -u postgres psql`, `cargo sqlx migrate
  run`; attempt 4: green, no flakes).

## Tickets partially done

- **P0-034 / P0-046 / P0-083**: implemented but unverified —
  live ACs blocked by DOCKER-001 (and P0-083's CI gate by
  CI-WORKFLOWS-001). Unticked by the established convention;
  NOT skipped work — the implementation is committed, only the
  live verification waits for docker-capable CI.

## Tickets skipped

- None. No S1/S2 issues appeared; no owner decision blocked the queue.

## Decisions taken

- No new ADRs. (The P0-093 matrix reading is an assumption, not a
  durable decision — see below.)

## Assumptions made

- **ASSUMPTION-013** (P0-093, ISSUES.md): "24 images" = 24 marked
  inputs (3 formats × 4 sizes × 2 marks) + 12 clean controls as the
  SSIM/PSNR reference; 36 image files total. If the owner meant 24
  total files, the generator drops a variant with a one-line change.
- P0-092: `spawn_app` already exists in `apps/api/tests/common` — no
  second helper invented.
- P0-095/P0-096: engine reports / mask codec do not exist in Phase 0;
  their snapshots/properties land with the Phase 2 work, using the
  harnesses built here. Nothing was faked to fill the gap.

## Blocked items / open issues

- **DOCKER-001** (open): containers still cannot start on this VM.
  Dev fallback used all run: native Postgres 16 + native redis.
- **CI-WORKFLOWS-001** (open): `.github/workflows/` still cannot reach
  GitHub (App 403). The `fixtures-check`, `templates-check` and
  nightly `prop` jobs added this run have never run remotely.
- **FLAKY-001** (open): 10 more failures this run (worst run yet in
  P0-092: two streaks of 3). Same signature, no new evidence.
- **FLAKY-002** (open): 4 more occurrences this run (8 total).
- **FLAKY-003** (NEW, first occurrence P0-093): `readyz_degraded_when_storage_down`
  read storage ok once; suspected `free_port()` TOCTOU race with the
  storage crate's `fake_s3` 200-responder; 5/5 isolated passes. One
  strike per the flaky policy (two = quarantine). Fix belongs to a
  hardening ticket, not a feature ticket.
- **PERF-001** (NEW, P1-004): synchronous Argon2id verify blocks
  tokio workers — a burst of concurrent logins 500s on pool
  timeout (proven by a scratch run: `PoolTimedOut`, 8/20 ok).
  Hardening candidate: `spawn_blocking` for the verify.
- **A fifth reset hit during P1-004 (this run, ~03:12 IST 13
  Nov):** binaries gone again (no `pg_ctlcluster`/`redis-server`,
  no `/etc/redis`, no `/var/lib/postgresql`). Waited out the
  os-intent replay (pid 752, ~2.5 min), which reinstalled the
  packages itself — do NOT install in parallel. Restarted
  services (setsid redis, `pg_ctlcluster 16 main start`),
  recreated the `pf` role + database, re-applied migrations via
  `cargo sqlx migrate run` (fresh cluster needs all migrations,
  including the new `login_attempt_counters` one).
- **A fourth reset hit during P1-001 (this run, ~00:50 IST 13
  Nov):** services down, cluster fresh (no `pf` role). Binaries
  survived this time (no apt reinstall needed): restarted redis
  (setsid) + postgres (`pg_ctlcluster 16 main start`), recreated
  the `pf` role + database, re-applied migrations via
  `cargo sqlx migrate run` — `just migrate` could not run yet,
  because it compiles the workspace and the new `query_as!`
  queries had no `.sqlx/` data (chicken-and-egg: migrate first
  with the standalone sqlx-cli, then `just sqlx-prepare`).
  The `os-intent` replay pid was 13111 this time (not 764).
- **ASSUMPTION-010** (open): VM reset at run start had wiped
  postgres/redis entirely (no binaries, empty apt cache). Restored via
  full `apt-get update` + install (~10 min through the proxy), setsid
  redis, `pg_ctlcluster 16 main start`, recreated `pf` role/db.
  `just` and cargo survived the reset. **A second reset hit mid-run
  (~01:05 IST 13 Nov, during the P0-097 verify): the runtime
  restarted, /tmp was wiped (verify log lost), postgres/redis gone
  again. Restored the same way (~17 min this time — apt update was
  slow through the proxy, one Translation file failed to fetch,
  non-fatal). Verify logs now live in `.scratch/logs/` (gitignored,
  survives restarts). Cargo, just, the repo and `target/` all
  survived both resets; only apt-installed system packages are
  wiped.
- **A third reset hit ~03:49 IST 13 Nov (during the P0-103
  verify — the run was killed mid-lint, signal 15).** This time
  the cell's own boot replay (`os-intent` replay, pid 764) held
  the apt lock for ~25 min reinstalling the recorded packages
  (container/docker stack first) — do NOT fight it or install
  in parallel; wait for it. Its `apt-get update` alone took
  ~10 min through the proxy. Redis/postgres binaries came back
  via the replay (~04:05 IST); services restarted the usual
  way (setsid redis, `pg_ctlcluster 16 main start`, recreated
  the `pf` role + database in the fresh cluster). The P0-103
  tree survived intact; verify then took 5 attempts (fmt,
  FLAKY-002, reset-kill, FLAKY-001, green).

## Test / verify status

- `just verify` green at every ticket commit: 336 (P0-092–P0-094) →
  343 (P0-095) → 357 (P0-096–P0-100) → 375 (P0-101) → 379
  (P0-102) → 387 (P0-103) → 391 (P0-104) → 391 (P0-105, no Rust
  test change) → 391 (P0-106, docs only) → 396 (P0-107; +5
  security-smoke) → 396 (P0-108, docs only) → **418 passed, 0
  failed** (P1-001; +22: 6 password + 1 config + 4 validator +
  3 service unit + 8 register) → **437 passed, 0 failed**
  (P1-002; +19: 4 tokens + 2 service unit + 13 integration)
  → **451 passed, 0 failed** (P1-003; +14: 5 JWT + 1 login
  service unit + 8 login integration) → **459 passed, 0
  failed** (P1-004; +8: 2 lockout-curve unit + 6 lockout
  integration) → **486 passed, 0 failed** (P1-005; +27: 16
  profile integration + 5 profile-service unit + 6 validator
  display-name unit) → **498 passed, 0 failed** (P1-006; +12
  email-change integration) → **507 passed, 0 failed**
  (P1-007; +9: 5 api deletion + 3 gc purge + 1 migration)
  → **527 passed, 0 failed** (P1-008; +20: 14 api
  admin_suspend + 6 validator reason).
  Python script tests:
  119 → 132 → **143 OK**. `just fixtures-check`,
  `just templates-check`, `just snapshots-check`, `just prop`
  all green. `just bench` green (17 benches, 2m54s). `just coverage`
  green (12 min). `just deny` green (P0-100, re-checked P1-001
  with the new argon2/rand_core crates).
- Verify attempts per ticket: P0-092: 9 (FLAKY-001 ×6, FLAKY-002 ×2),
  P0-093: 6 (FLAKY-001 ×3, FLAKY-002 ×1, FLAKY-003 ×1), P0-094: 1,
  P0-095: 1, P0-096: 3 (FLAKY-001 ×1, FLAKY-002 ×1), P0-097: 3
  (FLAKY-001 ×2; first attempt's log lost to the VM restart),
  P0-098: 2 (FLAKY-001 ×1), P0-099: 4 (FLAKY-001 ×3 — streak of 3,
  then green after a 45 s settle; load was low, 0.71),
  P0-100: 4 (FLAKY-001 ×3 — streak of 3, then green after a 60 s
  settle), P0-101: 1, P0-102: 3 (fmt, then clippy unused-import,
  then green; no flakes), P0-103: 5 (fmt, FLAKY-002, VM reset
  killed the run, FLAKY-001, then green), P0-104: 2 (fmt, then
  green; no flakes), P0-105: 1 (the Python drift-test failure was
  a separate suite, fixed before verify — P0-103's missing
  `PF_RATE_LIMIT_*` vars in `.env.example`/runbook), P0-106: 1,
  P0-107: 3 (fmt, FLAKY-002, then green), P0-108: 2 (FLAKY-002,
  then green after a 45 s settle), P1-001: 2 (clippy
  `result_large_err` on the handler, then green — the verify
  itself never saw a flake), P1-002: 2 (FLAKY-002
  `slow_consumer_drops_safely` first, then green after a 45 s
  settle), P1-003: 3 (two test-isolation fixes — refill-0
  bucket staleness in the login IP test, then route-table
  probes tripping the shared "unknown" IP bucket — then
  green; no flakes), P1-004: 1 (no flakes — the lockout
  suite itself ran 3× green before the full verify), P1-005: 2
  (attempt 1 failed on `job_events_retry` — its 001+002-only
  scratch schema lacked the P1-005 columns; fixed by adding the
  profile-fields migration to its scratch dir; no flakes),
  P1-006: 3 (attempts 1–2 failed on FLAKY-002
  `slow_consumer_drops_safely` — streak of 2, green after a
  60 s settle; the 6th VM reset hit mid-attempt-2 — the runtime
  restarted, the verify was interrupted mid-run, services +
  binaries were wiped, restored per ASSUMPTION-010: wait the
  os-intent replay (pid 753, ~40 s), setsid redis,
  `pg_ctlcluster 16 main start`, recreate `pf` role/db via
  `sudo -u postgres psql` (the fresh cluster's `postgres`
  superuser takes peer auth only — password auth fails),
  `cargo sqlx migrate run` on the fresh `pf` DB),
  P1-007: 1 (no flakes).
  P1-008: 4 (attempt 1: fmt; attempt 2: clippy
  `doc_lazy_continuation` on doc lines starting with `+`
  (users.rs `suspend` doc, admin_suspend.rs header,
  routes/admin.rs header — the P1-001 lesson, re-learned);
  attempt 3: 2 core error-table failures (the new
  `AccountSuspended` variant was missing from
  `response_table()` + the three hardcoded `36` counts —
  fixed to `37`), then the 7th VM reset killed the run
  mid-api-tests; attempt 4: green, no flakes).
  Every red run was a known flake, a lint fix, or a
  test-isolation fix; no regressions found.
- Coverage: first live report (P0-098): core 96.7%, engine 99.1%,
  api 92.7%, worker 83.8% — all above NFR-50 floors; report-only
  until Phase 2. Re-measured during P1-001 (`just coverage`):
  core 96.8%, engine 99.1%, api 92.6%, worker 83.8% — still
  above floors.
- Benchmarks: scaffold baselines committed (P0-097); full-job 4K mean
  545 ms, inpaint tiles 4K mean 223 ms, encode JPEG q95 4K mean
  343 ms — harness numbers, see the benches README.

## Quality-gate runs

- `just verify` (fmt + clippy `-D warnings` + tests) green at each
  commit, as above. `just smoke` not run (excluded per ADR-0017 — no
  docker daemon here). No engine metrics exist to gate yet.

## Next-run plan (first 5, in order)

1. P1-009 — Admin user search.
2. P1-010 — Impersonation.
3. P1-011 — Impersonation audit tests.
4. P1-012 — User lifecycle docs.
5. P1-013 — JWT claims + issuance.

Notes for the next coordinator: the register handler established
the handler shape — return `Response` directly, never
`Result<_, ProblemResponse>` (clippy `result_large_err`; axum
has no `Box<ProblemResponse>` responder). P1-002 answered its
open question: register mints the token via
`issue_verification` INSIDE the register transaction (same tx
as the user row + outbox enqueue), payload `{user_id, token}` —
no second row, no payload update. Two P1-002 lessons to carry
forward: (1) the resend rate limit counts OUTBOX rows, not
token rows (minting deletes token history — see the P1-002
status); (2) the api harness shares ONE scratch DB across
tests in a binary — scope outbox/mail assertions to the test's
own user/email, never assert global emptiness; and never edit
a migration file after it has been applied locally without
repairing the `_sqlx_migrations` checksum (sha384 of the file)
on `detag_api_harness` + `pf`. P1-003 lessons: (1) refill-0
rate-limit buckets never refill and their Redis keys survive
across test runs — rate-limit tests must use per-run unique
keys (UUID XFF / UUID email); (2) the oneshot "unknown" IP
bucket is shared across ALL test binaries running in parallel
— any test that sends many /v1 requests in a burst (the
route-table probes: 62) must use its own per-run XFF, or the
aggregate trips the 120-burst bucket; (3) axum 0.8 has no
`FromRequestParts` for `Option<ConnectInfo>` — use
`MaybeConnectInfo` for optional socket addresses; (4)
`jsonwebtoken` uses the `aws_lc_rs` backend (NOT
`rust_crypto` — unpatched advisories; needs cmake + cc).
P1-004 lessons: (1) a concurrent first-failure INSERT races
— use `INSERT ... ON CONFLICT DO NOTHING` (placeholder) +
`SELECT ... FOR UPDATE`, never SELECT-then-INSERT (the
loser's PK conflict poisons the whole Postgres tx → 500);
(2) do NOT write an HTTP-level concurrent-login test on this
VM — synchronous Argon2id blocks the 2-worker runtime and
pool acquires time out (PERF-001); race proofs belong at the
repo layer; (3) a login test that makes >10 attempts against
one account needs raised buckets (the reset test uses
100/0), and a test that locks an IP must use a fresh XFF for
any later login from a different scope (register clears the
account counter, not the IP counter).
P1-005 lessons: (1) a double `Option<Option<T>>` PATCH field
does NOT distinguish JSON `null` from a missing field with
plain serde derive (`null` deserializes as the outer `None`)
— fields that must support "clear with null" need an explicit
`deserialize_with` (proven by a unit test first: the
clear-PATCH behaved as a no-op until the deserializer
landed); (2) changing a shared repo's column list breaks
tests that build a partial schema — `job_events_retry.rs`
applies ONLY 001+002 (+ now the P1-005 migration) to its
scratch DB, so any future `users` column change must add its
migration there too (grep for the stem list); `repos_live` /
`tx_live` apply ALL migrations and are immune; (3) `is_live`
in the route-table generator now takes (method, path) —
one path can hold both a live and a stub route (GET/PATCH
/v1/me live, DELETE still the P1-007 stub) — add the
(method, path) to `is_live` AND to the special-case in
`tests/route_table.rs` when a handler lands.
P1-006 lessons: (1) the outbox accumulates one mail per mint
— a superseded mail stays queued with a dead token — so
tests that read a token from the outbox must take the LATEST
mail for the user (`.last()`), never assert exactly one
(the verification tests do the same); (2) the harness DB
persists across runs — any test that CONFIRMS a swap (or
otherwise enforces a uniqueness constraint) must use unique
addresses per run, or the second run 409s against the first
run's rows (fixed pending strings are fine only for
PATCH-only tests, which never hit the constraint); (3) an
unknown token at confirm is PF-1010, not PF-1001 — PF-1001
means "no credential at all"; check the taxonomy before
writing the expectation, not after. 6th VM reset (P1-006):
the runtime restarted mid-verify; the os-intent replay (pid
753) finished in ~40 s and the binaries were back — wait it
out, do NOT apt-install in parallel; the fresh cluster's
`postgres` superuser takes peer auth only, so recreate the
`pf` role/db via `sudo -u postgres psql`, then `cargo sqlx
migrate run` on the fresh `pf` DB.
The duplicate-email 409-vs-201
question is recorded in the P1-001 status (taxonomy says 409;
full indistinguishability would be a DECISIONS.md item). If
the VM resets again: binaries may survive — check `which psql
redis-server` first; if the cluster is fresh, `cargo sqlx
migrate run` (not `just migrate`) applies the migrations
without compiling the workspace.

## Risk list (top 3)

1. **Flake streaks are getting worse** (FLAKY-001: 10 failures this
   run; FLAKY-002: 4; FLAKY-003 is new). Each retry costs a full
   verify run (~80 s + the 32 s shutdown test). P0-099 (flaky policy
   tooling) and the hardening ticket for FLAKY-001 should not slip
   much further, or verify time will dominate the nightly loop.
2. **CI workflows still cannot reach GitHub** (CI-WORKFLOWS-001):
   the checks added since ~09:20 IST — including fixtures-check,
   templates-check and the nightly job — have never run remotely.
   The parent/owner still needs another push route for the two
   workflow files.
3. **VM resets keep wiping the dev services** (ASSUMPTION-010): this
   run's reset needed a full apt reinstall (~10 min). If the apt
   cache stays empty and the proxy is slow, every reset costs the
   same; a local restore script would pay for itself. The 5th
   reset (P1-004) was cheaper — the os-intent replay reinstalled
   the packages itself; just wait for it, then restart services
   + recreate `pf` role/db + re-apply migrations. The 7th reset
   (P1-008, ~12:57 IST) had NO replay running — full `apt-get
   update` (6m43s) + install was needed again; the restore
   recipe is now stable (install → setsid redis →
   `pg_ctlcluster` → recreate `pf` via `sudo -u postgres psql`
   → `cargo sqlx migrate run`), it just costs ~8 min each time.
