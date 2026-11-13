# PHASE 1 — Identity & Billing

> **Tickets: 111** · Types: feat 86 · chore 1 · test 14 · docs 10

**Goal:** Full SaaS core: registration/verification, JWT sessions with rotation, password flows, API keys, RBAC, plans + quotas + entitlements, Stripe billing (test mode) with Razorpay adapter behind a flag, usage metering, audit + abuse workflows, onboarding/consent (ownership attestation).

**Context (read before starting):**
- Stripe stays in TEST MODE; live keys are owner-input (DECISIONS.md queue).
- Security tests in 05-QUALITY/03-security-testing.md (authz matrix, refresh reuse) belong to THIS phase, not Phase 5.
- Ownership attestation is a hard gate (FR-C-01) - it lands here because uploads/jobs begin relying on it in Phase 2.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] Auth lifecycle e2e green: register, verify, login, refresh, rotate, reuse-attack revocation, logout.
- [ ] Authz matrix test with zero gaps across all live /v1 routes (including api-key paths).
- [ ] Stripe test-mode lifecycle green via test-clock: trial, active, past_due, canceled, webhook replay-safe.
- [ ] Quota enforcement race-safe; entitlements surface in /v1/me and headers.
- [ ] Attestation enforced on upload + job creation; audit trail verified append-only.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E1.1 Users & registration | auth/users | 12 |
| E1.2 Sessions & JWT | auth/sessions | 12 |
| E1.3 Password & email security | auth/password | 8 |
| E1.4 API keys | auth/apikeys | 9 |
| E1.5 RBAC & authz | auth/rbac | 7 |
| E1.6 Plans & entitlements | billing/plans | 10 |
| E1.7 Stripe billing | billing/stripe | 14 |
| E1.8 Razorpay adapter | billing/razorpay | 6 |
| E1.9 Usage metering | billing/metering | 8 |
| E1.10 Audit & abuse | audit | 7 |
| E1.11 Onboarding & consent | onboarding | 7 |
| E1.12 Email polish | mailer | 5 |
| E1.13 Support tooling | support | 6 |

---

## E1.1 — Users & registration (area: auth/users)

- [x] **P1-001** — Registration endpoint
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** POST /v1/auth/register per api-spec: validation, disposable-email denylist (config), argon2id hash (params from config), ToS version recorded, verification email queued via outbox; audit events register.attempt/.success.
  - **AC:**
    - [x] duplicate email generic error (no enumeration)
    - [x] outbox email queued
  - **Status:** DONE 2026-11-13 (local HEAD `0212574` + this commit; `just verify` green: 418 passed, 0 failed, attempt 2 — clippy `result_large_err` first, then green). New `crates/core/src/password.rs` (Argon2id, OWASP defaults m=19456/t=2/p=1, PHC string, random salt per hash, verify; 6 unit tests) — `argon2` 0.5 (+ `rand_core` 0.6 with `getrandom`, because argon2's `rand` feature does not enable rand_core's getrandom gate for `OsRng`). Config: 5 new `PF_*` vars (argon2 ×3, `PF_TOS_CURRENT_VERSION` "v1", `PF_DISPOSABLE_EMAIL_DOMAINS`) + validation + config test; `.env.example` (starter disposable list) + runbook + regenerated config table (`just env-check` OK). Also fixed in passing: the config test's `ALL_VARS` was missing the 4 `PF_RATE_LIMIT_*` vars (same leak-between-tests bug class) — added, now 51/51 in sync with FIELD_VARS. Validator: `validate_password` (min 12 chars, NIST SP 800-63B, never trimmed, counted in characters; 4 tests). Repos: `users::create_with_tos` (`create` unchanged — 10+ callers), new `audit_logs.rs` (insert + list_for_target/list_for_action) and `email_outbox.rs` (enqueue + unsent_for_template); `.sqlx/` regenerated (1 deleted old `create` query, 6 new). Service `apps/api/src/services/register.rs` (first real handler creates `services/`): normalize → policy checks (disposable `disposable_email`, ToS `tos_version_mismatch`, both 422) → hash → audit attempt (outside the tx, survives rollback) → one tx (user + success audit + outbox enqueue; duplicate = unique-constraint race → 409 `PF-2011` Conflict, generic body). Handler + `#[utoipa::path]` + ApiDoc (register path, RegisterRequest/UserResponse schemas; utoipa features `uuid`+`chrono` enabled) + `just openapi`/`just route-table` (route table now 3 live / 60 stubs; generator + probe test both learned the register exception — empty-body probe is 422 PF-2002). Harness: `spawn_with_fixtures_and_settings` (migrated scratch DB + custom settings). Tests: 8 new in `apps/api/tests/register.rs` (happy path incl. audit + outbox rows with no raw email, duplicate 409 + case-insensitive duplicate, disposable 422, ToS 422, structural 422s collected, normalization, malformed JSON plain 422, defaults). Handler-shape finding (P1-001): clippy `result_large_err` denies `Result<_, ProblemResponse>` (152 bytes vs a small Ok), and axum has no `IntoResponse` for `Box<ProblemResponse>` (tried, does not compile) — real handlers return `Response` directly (success tuple + ProblemResponse via `.into_response()`); the adding-an-endpoint doc was corrected to that prescription. Two test bugs found by running: `OutboxMail` not being Serialize (assertion rewritten to the email-bearing fields) and a doc-comment line starting with `+` (clippy `doc_lazy_continuation`). Honest notes: duplicate → 409 per the taxonomy's Conflict example, but the *status* distinguishes duplicate from new — full indistinguishability (201 for duplicates) would be a DECISIONS.md item; the verification token is P1-002's scope (the queued mail carries `{user_id}` only until then); password minimum 12 is a product-policy constant, not config (no doc specified it). Docs: api-spec register behavior paragraph; `just deny` green (new crates pass advisories/bans/licenses). Environment: a 4th VM reset hit mid-ticket (services down, cluster fresh — postgres/redis restarted, `pf` role + DB recreated, migrations re-applied via `cargo sqlx migrate run` since `just migrate` needs the code to compile first); `.sqlx/` data was regenerated after the new queries landed.

- [x] **P1-002** — Email verification flow
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-001 · **Prio:** P1
  - **Spec:** Single-use hashed token (24h), verify endpoint, resend rate-limited (3/hour), verified_at set; verification required before job creation (PF-1009).
  - **AC:**
    - [x] token single-use
    - [x] unverified cannot create jobs
  - **Status:** DONE 2026-11-13 (local HEAD `342c40c` + this commit; `just verify` green: 437 passed, 0 failed, attempt 2 — FLAKY-002 `slow_consumer_drops_safely` first, then green after a 45 s settle). Migration `20261113014639_email_verification_tokens` (id, user_id, token_hash sha256 UNIQUE, expires_at, used_at, created_at; `(user_id, created_at)` index). Core `tokens.rs` (32 random bytes, base64url 43 chars, sha256 hash, constant-time compare, TTL 24h as a constant; 4 unit tests) + repo `email_verification_tokens.rs` (mint, by_hash, atomic consume `UPDATE ... SET used_at = now() WHERE ... AND used_at IS NULL AND expires_at > now()`, delete_unused_for_user) + `users::set_email_verified`. Service `services/verification.rs` (issue/verify/resend/`require_verified`; 2 unit tests). Handlers `POST /v1/auth/verify-email` (204; unknown/used → 401 PF-1010, expired → 401 PF-1011) + `POST /v1/auth/resend-verification` (204-always; a NEW route — the spec says "resend rate-limited (3/hour)" but names no route; added by this ticket, recorded in the api-spec). Register now mints via `issue_verification` (outbox payload `{user_id, token}`; the register happy-path test now asserts the token row exists, unused, ~24h expiry). The PF-1009 gate `require_verified` is pure (on the loaded user, proven by its own test — job-create is a Phase 2 stub, so there is no live route to hook yet). Route table 64 entries (auth 17), 5 live / 59 stubs; OpenAPI gained both paths + both request schemas. Tests: 13 new in `tests/verification.rs` (happy path + audit, single-use, unknown 401, expired 401 + not consumed, concurrent double-verify exactly one 204, resend invalidates the old token + fresh verifies, rate limit 3/hour, unknown-email 204 + no mail, verified-user 204 + no mail, malformed email 422, empty token 422, malformed JSON 422, `require_verified` PF-1009/403 gate). Design bug caught by the rate-limit test: minting deletes previous unused tokens (one live token per user), so counting token rows for the 3/hour resend limit under-counts — the third resend minted a 4th mail. Fix: the limit counts `email_outbox` rows (template + user + hour) — every mint enqueues exactly one row in the same transaction, so the outbox count IS the mint count, 1:1, and the mailer never deletes rows. The token repo's `count_recent_for_user` was REMOVED (its `.sqlx/` file dropped, the new outbox count query prepared) rather than left to silently under-count; the migration header comment + data-model doc were corrected (the `(user_id, created_at)` index is for admin/debug listing, not the rate limit). Test bug: the unknown-email resend test asserted a globally empty outbox — the scratch DB is shared across tests in the binary, so the assertion is now scoped to the unknown email's hash. Migration-checksum lesson: editing an uncommitted migration's comment changes its sqlx checksum — the local DBs (`detag_api_harness` + `pf`) needed `UPDATE _sqlx_migrations SET checksum = decode(sha384(file), 'hex')` (the method was verified against an unmodified migration first); committed migrations must never be edited. Docs: api-spec verify/resend paragraph + resend row + register payload `{user_id, token}`; data-model doc; outbox repo header corrected to `{user_id, token}`.

- [x] **P1-003** — Login endpoint
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-002 · **Prio:** P1
  - **Spec:** Constant-time verify, generic error, session create + refresh cookie + access token; audit; per-IP + per-account rate limits wired to E0.11 limiter.
  - **AC:**
    - [x] no enumeration
    - [x] rate limit trips
  - **Status:** DONE 2026-11-13 (local HEAD `af7e4fc` + this commit; `just verify` green: 451 passed, 0 failed, attempt 3 — two test-isolation fixes first, then green; no flakes). Core `jwt.rs` (HS256 via `jsonwebtoken` 11 + `aws_lc_rs` backend, claims sub/role/exp/iat, leeway 0, `ACCESS_TOKEN_TTL_SECS` 900 / `REFRESH_TOKEN_TTL_SECS` 30d as product-policy constants; 5 unit tests) + repo `sessions.rs` (`create`, `list_for_user` — P1-007 reuses the latter). Service `services/login.rs`: normalize → per-account bucket (email sha256, 10 burst / 1 per min, checked BEFORE the password verify) → audit `login.attempt` → load user → Argon2id verify → one tx (session row + `login.success` audit) → issue access token; unknown email verifies against the state's precomputed dummy hash (`AppState::dummy_password_hash`, one hash at boot with the settings' params — so unknown email costs exactly one verify, like a wrong password) and fails with the same generic 401 `PF-1001` (the test proves the two bodies byte-identical); a malformed stored hash is a 500, never "wrong password"; `login.failure` audits the reason (`unknown_email` / `wrong_password`) internally only. Login does NOT require a verified email (verification gates job creation, PF-1009, not authentication). Handler sets the `detag_refresh` cookie (`HttpOnly; Secure; SameSite=Lax; Path=/v1/auth; Max-Age=2592000`); the refresh token never appears in the body. New settings `PF_RATE_LIMIT_LOGIN_ACCOUNT_BURST` (10) / `_REFILL_PER_MIN` (1) + validation + `.env.example` + runbook + regenerated config table (`just env-check` OK). Route table 64 entries, 6 live / 58 stubs; OpenAPI gained the login path + schemas. Tests: 8 new in `tests/login.rs` (happy path incl. token verify + cookie attrs + session row + audit, identical-401s, unverified can log in, second login = new family, account limit trips pre-auth with a correct password → 429 PF-3007 + Retry-After, IP limit trips, structural 422s, no password/hash leak) + 1 config test addition (login-account defaults + zero-burst). `just deny` green (see the backend note). Two findings: (1) jsonwebtoken's `rust_crypto` feature pulls `rsa 0.9.x` (RUSTSEC-2023-0071, patched in 0.10) + `crypto-bigint 0.5.5` (RUSTSEC-2026-0387, patched in 0.7.6) — neither has an upgrade path under that feature, so the crate uses the `aws_lc_rs` backend instead, which removes both crates from the tree (only patched `crypto-bigint 0.7.x` remains); it needs cmake + cc at build time (both exist here; verify before switching builders). (2) Test-isolation: refill-0 rate-limit buckets never refill and their Redis keys survive across runs — the login IP test now uses a per-run UUID XFF, and the route-table probes (62 /v1 requests in one burst) got the same treatment after they tripped the shared "unknown" 120-burst IP bucket (the login binary's ~30 parallel requests tipped it over). axum 0.8 has no `FromRequestParts` for `Option<ConnectInfo>` — the login handler uses a new never-rejecting `MaybeConnectInfo` extractor (`routes/extractors.rs`). Docs: api-spec login paragraph; security-testing login bullets (T17); threat model gained T17 (credential stuffing / brute-force login — LIVE bucket / PLANNED lockout P1-004).

- [x] **P1-004** — Lockout/backoff
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-003 · **Prio:** P1
  - **Spec:** Failed-attempt counters per account + per IP, exponential backoff windows, lockout notification email; resets on success and on password reset.
  - **AC:**
    - [x] backoff curve tested
    - [x] race-safe counters
  - **Status:** DONE 2026-11-13 (local HEAD `5457dab` + this commit; `just verify` green: 459 passed, 0 failed, attempt 1 — no flakes). Core `lockout.rs` (curve: threshold 5, window 15 min, first lock 60 s doubling to a 3600 s cap; 2 unit tests proving the curve exactly) + repo `login_attempts.rs` (`Counter`, `LockScope` account/ip, `record_failure` with `INSERT ... ON CONFLICT DO NOTHING` placeholder + `SELECT ... FOR UPDATE` + GREATEST lock extension, `mark_notified`, `reset`; `get` for the lock check) + migration `20261113030740_login_attempt_counters` (scope/key PK, attempts, first/last_failed_at, locked_until, notified_at). Error taxonomy: `AccountLocked` PF-1008, 423, title `account_locked`. Service `services/login.rs`: lock check (account + IP, later lock wins, `Retry-After` = max remaining, min 1 s) BEFORE the password verify — locked attempts are not audited and not recorded; failure path is one tx (both counters + `login.failure` audit + on the first lock the `lockout-notice` outbox enqueue + `mark_notified` + `login.locked` audit); success tx resets both counters; `client_ip` via the new `rate_limit::client_ip_from` (same XFF-first identity as the rate limiter; IP counter skipped for `"unknown"`). Handler renders 423 PF-1008 + `Retry-After`. Register clears the account counter in its tx (an attacker cannot pre-lock an address the owner registers later). Tests: 6 new in `tests/lockout.rs` (5-failure lock with 60 s backoff + no extension while locked, reset on success, notification once per episode + `login.locked` audit, IP lockout blocks a fresh account, register clears a pre-existing counter, repo-level 20-concurrent records never lose an increment — counter exactly 20, lock at the 1-hour cap). Two findings: (1) the placeholder INSERT was needed — a plain SELECT-then-INSERT races (two concurrent first-failures both INSERT; the loser dies on the PK conflict and a failed statement poisons the whole Postgres tx → 500); (2) an HTTP-level concurrent race test (20, then 8, then 4 concurrent logins) flaked on this VM — synchronous Argon2id verify blocks the 2-worker runtime, pool acquires time out (`PoolTimedOut`, proven by a scratch run), the login 500s — recorded as PERF-001 (hardening: `spawn_blocking` for the verify); the race proof therefore lives at the repo layer. Docs: api-spec lockout paragraph; data-model `login_attempt_counters` row; security-testing lockout bullets; threat model T17 now LIVE (lockout). Password-reset wiring: `reset` exists, the reset-password ticket wires the call (the endpoint is still a stub).

- [x] **P1-005** — Profile GET/PATCH /v1/me
  - **Type:** feat · **Area:** auth/users · **Est:** S · **Deps:** P1-004 · **Prio:** P1
  - **Spec:** Profile + plan + quota snapshot; patch display name and email-change intent; quota snapshot from entitlement service.
  - **AC:**
    - [ ] snapshot matches quota middleware — NOT verifiable yet: the middleware is P1-051 (~46 tickets later). The snapshot is built from the same two sources P1-051's spec names (plan entitlements + usage_counters) via the shared entitlements service, and P1-051 must verify agreement when the middleware lands (same honest-scope pattern as P0-104).
  - **Status:** DONE 2026-11-13. Migration `user_profile_fields` adds `display_name` + `pending_email` (no unique constraint by design — the P1-006 swap enforces uniqueness via `email`). `users::update_profile` (plain SET, so a PATCH can clear a field) + `subscriptions`/`usage_counters` repos + `validate_display_name` (trim, 1–100 chars, no control chars) + `entitlements` service (current plan row + quota snapshot from plan entitlements + today's usage row; missing/mistyped/negative limit reads as 0, fail closed). Auth extractor Bearer leg: verifies the JWT (leeway 0), loads the `sub` user, yields a verified context (user + role from the row); no credential / API key / session cookie → 401 PF-1001, malformed JWT → PF-1010, expired → PF-1011, valid JWT for unknown user → PF-1001. API-key + cookie legs, precedence + CSRF docs are P1-016's; user status is NOT checked here (suspension is P1-008's scope). `GET /v1/me` returns profile + plan + quota; `PATCH /v1/me` sets the display name and/or records the email-change intent (validated + disposable-checked + duplicate-checked → 409 PF-2011; unchanged → 422 `email_unchanged`); `null` clears, absent leaves unchanged, no fields = no-op 200 (no write, no audit); update + `profile.update` audit in one tx (pending email as sha256 in the audit). Route table 8 live / 56 stubs (DELETE /v1/me stays a stub for P1-007); OpenAPI gains both paths + 5 schemas. Serde trap found by the failing test: plain derive collapses JSON `null` into missing for double-`Option` PATCH fields — a custom `deserialize_with` keeps `null` = clear. 16 integration tests in `tests/profile.rs` (auth rejections, happy path incl. seeded usage 3/10 images, PATCH rules incl. audit + no-op) + 5 profile-service unit tests + the extractor's rejection unit test. `just verify` green 486 passed, 0 failed, attempt 2 (attempt 1 failed on `job_events_retry` — its 001+002-only scratch schema lacked the new columns; fixed by adding the profile-fields migration to its scratch dir; no flakes). Docs: api-spec profile + extractor paragraphs, data-model users row, security-testing profile line.

- [x] **P1-006** — Email change flow
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-005 · **Prio:** P1
  - **Spec:** PATCH triggers re-verification to new address (token), old-address notification (takeover alert), swap on verify; pending-state handling tested.
  - **AC:**
    - [x] old address alerted
    - [x] pending state safe
  - **Status:** DONE 2026-11-13. Migration `email_change_token_purpose` adds `purpose` (`verification` | `email_change`, CHECK, backfilled `verification`) to `email_verification_tokens`; the two purposes are never interchangeable (a registration token at confirm → 401 PF-1010, not consumed). `tokens::mint` takes a purpose + `delete_unused_for_purpose` (purpose-scoped delete); `users::swap_email` (sets email = pending, clears pending, sets email_verified_at, guarded by `pending_email IS NOT NULL`). `issue_email_change` (in the profile PATCH tx when a NEW pending address is set): deletes all unused tokens (one live token per user), mints an `email_change` token, queues `email-change-verify` to the new address (`{user_id, token}`) + `email-change-alert` to the old address (`{user_id}` — no token, never names the new address); `clear_email_change_tokens` kills the live change token when the intent is cleared (registration token survives). `POST /v1/auth/confirm-email-change` → 204: consume + swap + `email.change` audit (both addresses as sha256) in one tx; unknown/used/wrong-purpose → 401 PF-1010, expired → 401 PF-1011, address taken meanwhile → 409 PF-2011 (swap is the enforcement point; the failed swap rolls back, token stays unused, intent stays pending). Re-PATCHing the same pending address is a no-op (no re-mail — re-trigger by clearing + re-setting). One-live-token consequences recorded: a resend kills a live change token (intent survives); a change PATCH kills a live registration token. Route table 65 routes, 9 live / 56 stubs; OpenAPI gains the confirm path + schema. 12 integration tests in `tests/email_change.rs` (trigger mails, happy-path swap + audit, unknown/used/expired/wrong-purpose, cleared intent, taken address, display-name-only no-mail, same-pending-once, replace-pending, resend-kills-token) — all green twice in a row. `just verify` green 498 passed, 0 failed, attempt 3 (attempts 1–2: FLAKY-002 `slow_consumer_drops_safely` streak of 2, green after a 60 s settle; the 6th VM reset hit mid-attempt-2 — runtime restart, services restored per ASSUMPTION-010). Docs: api-spec email-change paragraph + route-table row, data-model tokens row, security-testing email-change line, threat model T18.

- [x] **P1-007** — Account deletion
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-006 · **Prio:** P1
  - **Spec:** DELETE /v1/me: status deletion_pending, 30-day grace with cancel-on-login, nightly purge job (cascade per data-model), audit + confirmation email.
  - **AC:**
    - [x] purge cascades per doc
    - [x] grace cancel works
  - **Status:** DONE 2026-11-13. Core repo `users.rs`: `DELETION_GRACE_DAYS` 30 (product-policy constant, not config), `request_deletion` (status + `deleted_at = now()`), `cancel_deletion` (active + `deleted_at = NULL` only where pending, `Option<User>` — `None` = already cancelled, not an error), `purgeable` (pending + `deleted_at < cutoff`, oldest first), `delete_user` (hard delete, `false` when already gone). Deletion service `services/deletion.rs`: `request_account_deletion` (one tx: status + `account.delete_requested` audit + `account-deletion` confirmation mail, payload `{user_id}`; second request while pending is a no-op — grace runs from the FIRST request) + `cancel_deletion_in_tx` (login hook's tx). `DELETE /v1/me` handler (204, Bearer extractor, `deleted_at` added to the profile response so the grace is visible via `GET /v1/me`); login cancels a pending deletion in the success tx (after the lockout resets, with the new session — a concurrent cancel first is not an error). gc purge `apps/gc/src/purge.rs`: `purge_expired_users` (dry-run default, one tx per user: `account.purged` audit + hard delete, run continues past per-user failures) + `detag-gc --purge-users` mode (runs instead of the artifact sweep; needs the DB only — S3 objects become orphans for the next sweep, rows first/objects after is the safe order). Migration `20261113062352_purge_cascade_triggers` — the purge test PROVED a cascade-vs-trigger conflict (purge_one failed with "table audit_logs is append-only"): (1) `audit_logs.actor_user_id` lost its FK (the actor is a historical fact, not a live reference — the FK's `ON DELETE SET NULL` is an UPDATE the trigger forbids, so no user with audit rows could ever be purged; the purge now leaves the actor id intact, a stronger trail than NULL); (2) `job_events` / `attestations` triggers are UPDATE-only (history is never rewritten; DELETE is the purge's erasure path — keeping those rows would retain the user's data after erasure, and no app code deletes them directly); (3) `audit_logs` trigger keeps its DELETE branch (the audit trail is never deleted). Down migration restores the FK `NOT VALID` (a plain re-add would fail validation against purged users' retained actor ids) + the DELETE branches. Route table 65 routes, 10 live / 55 stubs; OpenAPI gains the DELETE path. Tests: 5 new in `tests/deletion.rs` (happy path incl. audit + mail, idempotent re-request, auth required, login cancels, second login after cancel audits nothing more) + 3 gc purge tests (cascade incl. job_events + attestations + upload, grace/active kept, dry-run deletes nothing) + 1 migration test `migration_purge_cascade_triggers.rs` (FK gone, trigger defs, UPDATE fails / DELETE contract, user-delete cascade with audit row surviving with actor intact, revert + re-apply). Two test bugs found by running: `subscriptions.provider` + `idempotency_keys.endpoint` are NOT NULL (seed fixed). Lessons: (1) `just sqlx-prepare` does not compile `#[cfg(test)]` code — macro queries in tests have no offline data, so test SQL uses the literal function form; (2) sqlx 0.9 rejects dynamically built SQL (`SqlSafeStr`) — the gc test's count helper uses explicit per-table literal queries; (3) the 004 migration test pins its migration set and its old trigger contract by design — the new contract gets its own test file, not an edit of the 004 assertions (a header note points at it). Docs: api-spec deletion paragraph + route row, data-model cascade/retention + audit actor + trigger notes, security-testing deletion line, threat model T19 (deletion + erasure — LIVE), runbook gc purge mode. `just verify` green: 507 passed, 0 failed, attempt 1 (no flakes).

- [x] **P1-008** — Admin suspend/restore
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-007 · **Prio:** P1
  - **Spec:** POST /v1/admin/users/{id}/suspend|restore with reason; suspended sessions revoked immediately; queued jobs held; audited.
  - **AC:**
    - [x] sessions revoked
    - [x] jobs held
  - **Status:** DONE 2026-11-13. Error taxonomy: `AccountSuspended` PF-1003, 403, title `account_suspended` (PF-1003/1004/1005/1006 were all free). Validator `validate_reason` (trim, 1–500 chars, no control chars; codes `required`/`too_long`/`invalid_reason`) + 6 unit tests. Core repos: `users::suspend` + `users::restore` (status flip + `deleted_at = NULL`, `NotFound` when missing); `sessions::revoke_all_for_user` (sets `revoked_at` where NULL, returns count); `jobs::hold_queued_for_user` (`queued` → `held`) + `jobs::release_held_for_user` (`held` → `queued`). `AdminContext` extractor (`apps/api/src/auth.rs`): wraps `AuthContext`, requires `role == Role::Admin`, else 403 PF-1002. Service `services/admin.rs`: `suspend_user` / `restore_user`, `AdminError` (Validation/NotFound/Internal), `AdminActionOutcome` (user + already + counts); each action is one tx (status + revoke/hold/release + audit); suspend audit `user.suspended` (actor = admin, `after` carries `{status, sessions_revoked, jobs_held}`, reason in the audit's `reason`, `before` carries prior status + `deleted_at`); restore audit `user.restored` (`after` carries `{status: active, jobs_released}`); suspend is idempotent no-op when already suspended, restore is no-op when status != `suspended`; suspend cancels the deletion grace (`deleted_at` cleared — invariant: `deleted_at` non-NULL exactly when status is `deletion_pending`); restore sets status `active` + clears `deleted_at`; sessions stay revoked on restore (user logs in again); no suspension mail (spec does not ask). Login blocks suspended users (P1-008): AFTER the password verify succeeds and BEFORE the session tx — a wrong password is still the generic 401 (no info leak), a correct password gets 403 PF-1003 (`LoginError::Suspended` + handler mapping); `login.failure` audited with reason `suspended` (internal only, no lockout counter); lockout check stays first (suspended + locked → 423); the extractor still does NOT check status (residual access token ≤ 15 min is the documented design — threat-model trade-off). Handlers in `routes/admin.rs` (thin, return `Response` directly; suspend takes `ValidatedJson<SuspendRequest>`, restore takes no body; 200 with reused `UserResponse`); route table 65 routes, 12 live / 53 stubs; OpenAPI gains both paths + `SuspendRequest` schema. Tests: 14 new in `tests/admin_suspend.rs` (happy path incl. audit counts + sessions revoked + jobs held; non-admin 403 PF-1002; unauthenticated 401 PF-1001; unknown user 404 PF-2010; missing/blank reason 422 PF-2002; malformed UUID 422 PF-2002; already-suspended no-op; restore happy path incl. jobs released + sessions stay revoked; restore active no-op; restore auth rejections; unknown restore 404; suspend cancels pending deletion; suspended login 403 PF-1003 / wrong password 401 + audit; login works after restore). Bonus finding: the P1-008 malformed-UUID test caught a real error-mapper bug — the mapper kept the framework's 400 wire status while the problem body said 422 (its own docs already promised "a framework 400 becomes a 422 problem"); the mapper now stamps the wire status from the problem (404/413/415 mappings unaffected — their problems carry the same status). Three compile fixes found by running: `ValidationErrors` has no `add`/`into_result` (use `default()` + `push` + `is_empty`, the profile pattern), `validate_reason` was not re-exported from the validator crate root, and the rewritten `routes/admin.rs` had dropped the `routing::{get, post}` + `Router` imports. Two more fixes found by verify: clippy `doc_lazy_continuation` on doc lines starting with `+` (users.rs `suspend` doc, admin_suspend.rs header, routes/admin.rs header — the P1-001 lesson, re-learned), and the new variant was missing from core's error test tables (`response_table()` row + all three hardcoded `36` counts → `37`: `all_variants`, `response_table` length, unique-codes). Docs: api-spec admin section + login paragraph, data-model jobs row (`held` + Phase 2 claim note) + users row (`deleted_at` invariant), security-testing suspend bullets, threat model T20 (suspension bypass / admin-action abuse — LIVE) + residual-token trade-off. `just verify` green: 527 passed, 0 failed, attempt 4 (attempt 1: fmt; attempt 2: clippy `doc_lazy_continuation` ×2 files; attempt 3: 2 core error-table failures, then the 7th VM reset killed the run mid-api-tests — environment restored per ASSUMPTION-010, apt update 6m43s; attempt 4: green, no flakes).

- [x] **P1-009** — Admin user search
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-008 · **Prio:** P1
  - **Spec:** GET /v1/admin/users?query= (email/name/id prefix), cursor pagination, fields incl. plan, status, usage today; support role read-only.
  - **AC:**
    - [x] pagination + filter tests
  - **Status:** DONE 2026-11-13. Core repo `users::search_users` (one query, no N+1): prefix match (case-insensitive `ILIKE`) on email, display name, or id — a `None` query lists everything; plan code from the live subscription (`trialing`/`active`, newest first) else `free` via `COALESCE`; today's usage via a `LEFT JOIN` on `usage_counters` (UTC day, zeros when absent — the read never creates the row). Keyset pagination on `(created_at, id)` with `date_trunc('milliseconds', created_at)` on both sides of the comparison and in the `ORDER BY` — the `Cursor` stores Unix millis (P0-068) while `created_at` carries microseconds, so comparing the raw timestamp would re-include the cursor's own row or skip a same-millisecond row with a smaller id. `LIKE` metacharacters (`%`, `_`, `\`) are escaped (`escape_like_prefix` + unit test). sqlx infers the `COALESCE` columns as nullable (conservative analysis of the scalar subquery / LEFT JOIN), so `UserSearchRow` carries them as `Option` and the service unwraps with the documented defaults (`free` / 0). `AdminReadContext` extractor (`apps/api/src/auth.rs`): admits Admin + Support, else 403 PF-1002 — support is read-only (readable admin routes take this, writes take `AdminContext`). Service `services/admin.rs`: `search_users` (clamp_limit 1..=100 default 20; tampered cursor → `AdminError::InvalidCursor` → 422 PF-2002; blank query is no filter; `limit + 1` rows → `Page::from_window` with `timestamp_millis()` cursors) + `UserSearchItem` / `UserSearchPage` (the pagination shape; exists so the response has an OpenAPI schema — core's generic `Page<T>` carries no `ToSchema`). Handler `GET /v1/admin/users` (thin, `Query<SearchUsersQuery>`; non-numeric `limit` is rejected by the `Query` extractor → mapper → 422 PF-2002). Route table 65 routes, 13 live / 52 stubs; OpenAPI gains the path + 2 schemas. Tests: 11 new in `tests/admin_search.rs` (list incl. plan/status/usage fields; email/name/id prefix filters; no-match empty; pagination walks 3 pages without duplicates; limit=0 clamps to 1; tampered cursor 422; bad limit 422; support 200 / plain user 403 PF-1002 / unauthenticated 401 PF-1001; live Pro plan + today's usage 3/120 shown, Free + zeros otherwise), green twice in a row. Two test bugs found by running: (1) the first `unique_email` helper had no UUID — every test registered the same `admin@example.com` and 409d (the admin_suspend pattern appends a UUID); (2) the email local part exceeded the validator's 64-char cap (a full UUID in the prefix + a full UUID in the email is 80 — the prefix now carries the full UUID and the email carries none) — and the 8-hex prefix first tried as the fix recurred across runs within ~65 s (top of the unix-ms timestamp), so the previous run's users leaked into the filtered search until the prefix carried a full UUID. Docs: api-spec admin search paragraph + route-table row, security-testing search bullets. `just verify` green: 539 passed, 0 failed, attempt 1 (no flakes).

- [x] **P1-010** — Impersonation
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-009 · **Prio:** P1
  - **Spec:** POST /v1/admin/users/{id}/impersonate (reason required): 15-minute scoped token with impersonator_id claim, banner requirement documented, full audit, auto-expiry enforced.
  - **AC:**
    - [x] token expires 15m
    - [x] banner contract documented
  - **Status:** DONE 2026-11-13. Core JWT: `AccessTokenClaims.impersonator_id: Option<Uuid>` (`#[serde(default, skip_serializing_if)]` — normal tokens omit it, pre-P1-010 tokens without the field still verify) + `issue_impersonation_token(secret, user_id, role, impersonator_id, ttl_secs)` + 3 unit tests (carries the impersonator + exactly 900 s; normal token has None; expired impersonation token → `JwtError::Expired`). `AuthContext.impersonator_id: Option<Uuid>` (None in `from_headers`; set from the claims in the verified construction — the `role` is the TARGET user's role). Both admin guards reject any context carrying `impersonator_id` with 403 PF-1002 (`AdminContext` + `AdminReadContext`, writes AND reads — the token cannot suspend anyone, read the admin list, or impersonate someone else). Service `services/admin.rs`: `impersonate_user` (reason via `validate_reason`; target loaded, 404 when missing; token issued BEFORE the audit insert — the audit records a token actually handed out; an audit failure returns 500 and the client never learns the token; `user.impersonated` audit with actor = admin, target = user, reason verbatim, `after: {expires_in: 900}`) + `ImpersonateResponse {access_token, token_type: "Bearer", expires_in: 900, impersonator_id, user_id}`. No session row, no cookie, no refresh — the token dies at `exp` (P1-014's refresh must reject it). Any user may be impersonated, including another admin — the audit is the control, not a restriction the spec does not name. `SuspendRequest` renamed to `ReasonRequest` (shared admin-action reason body — suspend + impersonate). Handler `POST /v1/admin/users/{id}/impersonate` (AdminContext — support gets 403 PF-1002, it is a write). Route table 65 routes, 14 live / 51 stubs; OpenAPI gains the path + `ImpersonateResponse` schema (`SuspendRequest` schema renamed to `ReasonRequest`). Tests: 8 new in `tests/admin_impersonate.rs` (happy path incl. claims verified directly + audit; token acts as the user on GET /v1/me; token cannot reach admin endpoints — suspend 403 + user list 403; support/plain-user 403 + unauthenticated 401 + no audit for rejections; unknown user 404; missing/blank reason 422; malformed UUID 422; expired impersonation token → 401 PF-1011). Docs: api-spec impersonation paragraph (banner contract + per-action audit convention for P1-011 + refresh note for P1-014), security-testing impersonation bullets, threat model T21 (impersonation abuse — LIVE, P1-011 proves the audit rows + admin block + expiry). `just verify` green: 550 passed, 0 failed, attempt 1 (no flakes).

- [x] **P1-011** — Impersonation audit tests
  - **Type:** test · **Area:** auth/users · **Est:** M · **Deps:** P1-010 · **Prio:** P1
  - **Spec:** Every impersonated action logged with both ids; impersonator cannot reach admin endpoints while impersonating; expiry enforced.
  - **AC:**
    - [x] audit rows complete
    - [x] admin blocked during impersonation
  - **Status:** DONE 2026-11-13. The one code change this test ticket needed: the audit actor is threaded through the profile + deletion services — `AuthContext::audit_actor_id(user)` (the impersonator when impersonating, else the user) + `patch_profile(state, user, actor_user_id, req)` + `request_account_deletion(state, user, actor_user_id)`; the handlers pass it explicitly, and the `let Some(ref user)` borrow keeps `auth` usable after the unwrap (a moved `user` made `auth` unusable — the first compile failed with E0382 on both handlers). The other audited flows (register, verify, login, admin actions) need no change: register/verify/login take no Bearer token, and the admin guards already block impersonation. Tests: 7 new in `tests/admin_impersonate_audit.rs` (impersonated profile update audits both ids + the action works as the user; normal profile update still audits actor == target; impersonated deletion request audits both ids + the account is deletion-pending; the `user.impersonated` row checked field by field; admin block on every live admin route — restore + impersonate + user list, on top of P1-010's suspend; expired token on admin endpoints is 401 PF-1011, not 403; impersonating an admin target is still blocked while user routes work as the target). Docs: security-testing impersonation-audit bullets, threat model T21 now proven (LIVE) + update-log row. `just verify` green: 557 passed, 0 failed, attempt 1 (no flakes).

- [x] **P1-012** — User lifecycle docs
  - **Type:** docs · **Area:** auth/users · **Est:** S · **Deps:** P1-011 · **Prio:** P1
  - **Spec:** State diagram (active/suspended/deletion_pending), retention per NFR-40, admin procedure notes.
  - **AC:**
    - [x] diagram + notes committed
  - **Status:** DONE 2026-11-13. New `docs/03-ARCHITECTURE/06-user-lifecycle.md`: Mermaid `stateDiagram-v2` (active / suspended / deletion_pending / purged-terminal) with every transition labeled by actor + ticket; a what-each-state-allows table (login, jobs, profile, deletion, suspend/restore, purge per state); retention per NFR-40 as a table mapping each item to its enforcer + status; admin procedure notes (suspend/restore, search, impersonate + banner, admin creation, locked-vs-suspended triage, the no-admin-cancel for a pending deletion). Every transition is test-proven (P1-007/P1-008/P1-011) except suspended → deletion_pending, which was verified live by a scratch test during this ticket (a suspended user's DELETE returns 204, status becomes `deletion_pending` with a fresh grace — the scratch file was deleted after the run). Two honesty corrections found while writing: the upload `pending_parts` abort has no enforcing job (the state machine allows it, nothing runs it — marked TARGET, not LIVE) and the retention table's live rows are only the artifact sweep + the user purge (the other six are targets enforced by P5-017); the reset flow is P1-025 (named, not "a later ticket"). Linked from the data-model retention section. `just verify` green: 557 passed, 0 failed, attempt 1 (no flakes — docs only, no code change).

---

## E1.2 — Sessions & JWT (area: auth/sessions)

- [ ] **P1-013** — JWT claims + issuance
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** jsonwebtoken: claims {sub, role, ver, impersonator?, jti, exp 15m, iat, iss, aud, kid}; key from PF_JWT_SECRET; errors map to PF-1010/1011.
  - **AC:**
    - [ ] claims shape locked by test

- [ ] **P1-014** — Refresh rotation
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-013 · **Prio:** P1
  - **Spec:** 30d opaque refresh (hashed), family_id lineage; rotation replaces token; REUSE of a rotated token revokes the whole family (PF-1007) + security email.
  - **AC:**
    - [ ] reuse-attack test revokes family

- [ ] **P1-015** — Sessions table + repo
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-014 · **Prio:** P1
  - **Spec:** Per data-model; cleanup worker purges expired rows nightly; indexes per doc.
  - **AC:**
    - [ ] cleanup job green

- [ ] **P1-016** — Cookie + bearer dual auth
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-015 · **Prio:** P1
  - **Spec:** Web: httpOnly Secure SameSite=Lax refresh cookie + in-memory access; API: bearer only; precedence documented; CSRF posture documented (same-site + no state-changing GET).
  - **AC:**
    - [ ] both paths tested
    - [ ] posture documented

- [ ] **P1-017** — Logout endpoints
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-016 · **Prio:** P1
  - **Spec:** Revoke current session; logout-all revokes every session; audited; revoked refresh cannot rotate.
  - **AC:**
    - [ ] revocation enforced

- [ ] **P1-018** — Revoke on password change
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-017 · **Prio:** P1
  - **Spec:** Reset/change-password revokes all sessions except current; new-login digest email; tested.
  - **AC:**
    - [ ] sessions revoked
    - [ ] digest emailed

- [ ] **P1-019** — Session listing
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-018 · **Prio:** P1
  - **Spec:** GET /v1/me/sessions with device (UA parsed, ip-hashed), created, last_seen (sliding, throttled update); DELETE one session.
  - **AC:**
    - [ ] throttled last_seen verified

- [ ] **P1-020** — Brute-force limiter wiring
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-019 · **Prio:** P1
  - **Spec:** Redis token buckets per account+ip on login/register/reset; standard headers; lockout integrates backoff; concurrency tests.
  - **AC:**
    - [ ] headers present
    - [ ] concurrency safe

- [ ] **P1-021** — Token revocation cache
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-020 · **Prio:** P1
  - **Spec:** Short-lived jti denylist in redis consulted by extractor; password-change bumps denylist epoch; p99 unaffected (cache hit path).
  - **AC:**
    - [ ] denylist consulted
    - [ ] perf note recorded

- [ ] **P1-022** — JWT verification extractor
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-021 · **Prio:** P1
  - **Spec:** Full AuthContext: verify sig+exp+nbf, role refreshed from db for admin routes, api-key path integration (E1.4).
  - **AC:**
    - [ ] tampered token rejected
    - [ ] role refresh tested

- [ ] **P1-023** — Auth integration suite
  - **Type:** test · **Area:** auth/sessions · **Est:** M · **Deps:** P1-022 · **Prio:** P1
  - **Spec:** Lifecycle: register, verify, login, refresh, rotate, reuse-attack, logout; clock-injected expiry tests; token tampering rejected.
  - **AC:**
    - [ ] full suite green

- [ ] **P1-024** — Auth design notes
  - **Type:** docs · **Area:** auth/sessions · **Est:** S · **Deps:** P1-023 · **Prio:** P1
  - **Spec:** Threat-model update: token theft, replay, family revocation rationale; cookie vs bearer trade-offs.
  - **AC:**
    - [ ] notes committed

---

## E1.3 — Password & email security (area: auth/password)

- [ ] **P1-025** — Forgot/reset flow
  - **Type:** feat · **Area:** auth/password · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Request always 204 (no enumeration), hashed single-use 30-min token, reset revokes sessions, audit + email.
  - **AC:**
    - [ ] no enumeration
    - [ ] token single-use

- [ ] **P1-026** — Password policy + pwned check
  - **Type:** feat · **Area:** auth/password · **Est:** S · **Deps:** P1-025 · **Prio:** P1
  - **Spec:** Min length 12, strength score (crate), optional HIBP k-anonymity range check behind flag (privacy note), top-10k blocklist.
  - **AC:**
    - [ ] policy tested
    - [ ] flag default off

- [ ] **P1-027** — Argon2 params config
  - **Type:** feat · **Area:** auth/password · **Est:** S · **Deps:** P1-026 · **Prio:** P1
  - **Spec:** m=64MB t=3 p=4 defaults, configurable, rehash-on-login upgrade path; benchmark note per target VPS.
  - **AC:**
    - [ ] rehash path tested

- [ ] **P1-028** — Mailer abstraction
  - **Type:** feat · **Area:** auth/password · **Est:** M · **Deps:** P1-027 · **Prio:** P1
  - **Spec:** crates/mailer: lettre SMTP + outbox drain worker; retries with backoff; MailHog dev; template engine (tera).
  - **AC:**
    - [ ] drain worker green
    - [ ] retry tested

- [ ] **P1-029** — Email templates v1
  - **Type:** feat · **Area:** auth/password · **Est:** M · **Deps:** P1-028 · **Prio:** P1
  - **Spec:** Verify-email, reset-password, password-changed, new-login, lockout, verification-thanks: HTML + plain text; copy passes honesty rule.
  - **AC:**
    - [ ] templates snapshot-tested

- [ ] **P1-030** — Email flow tests
  - **Type:** test · **Area:** auth/password · **Est:** M · **Deps:** P1-029 · **Prio:** P1
  - **Spec:** Outbox assertions (template, recipient, token shape); mailhog e2e for verify + reset; no real SMTP in CI.
  - **AC:**
    - [ ] flows e2e green

- [ ] **P1-031** — Email send rate limits
  - **Type:** feat · **Area:** auth/password · **Est:** S · **Deps:** P1-030 · **Prio:** P1
  - **Spec:** Per-recipient hourly caps per template class (verify 3, reset 3, marketing 0); abuse log entries on trips.
  - **AC:**
    - [ ] caps enforced

- [ ] **P1-032** — Email ops docs
  - **Type:** docs · **Area:** auth/password · **Est:** S · **Deps:** P1-031 · **Prio:** P1
  - **Spec:** Deliverability checklist (SPF/DKIM/DMARC placeholders), sender config, test addresses, transactional-only stance v1.
  - **AC:**
    - [ ] docs committed

---

## E1.4 — API keys (area: auth/apikeys)

- [ ] **P1-033** — Key generation
  - **Type:** feat · **Area:** auth/apikeys · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** POST /v1/api-keys: pf_live_ prefix + 24B url-safe secret shown once; sha256 stored; scopes (jobs:write, jobs:read, webhooks:manage); max 10 keys/user.
  - **AC:**
    - [ ] secret shown once
    - [ ] cap enforced

- [ ] **P1-034** — Key auth extractor
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-033 · **Prio:** P1
  - **Spec:** Bearer pf_live_* path: hash lookup, scope check, throttled last_used_at, per-key rate tier.
  - **AC:**
    - [ ] scope check enforced

- [ ] **P1-035** — Key rotation
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-034 · **Prio:** P1
  - **Spec:** POST /v1/api-keys/{id}/rotate: new secret; old valid for 24h overlap (config); audited.
  - **AC:**
    - [ ] overlap window tested

- [ ] **P1-036** — Key revoke
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-035 · **Prio:** P1
  - **Spec:** DELETE immediate; cached key entries invalidated via redis denylist; tested.
  - **AC:**
    - [ ] revocation immediate

- [ ] **P1-037** — Per-key rate limits
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-036 · **Prio:** P1
  - **Spec:** Tier from config (default lower than user tier); 429 headers; abuse log on repeated trips.
  - **AC:**
    - [ ] tier enforced

- [ ] **P1-038** — Key-scoped usage attribution
  - **Type:** feat · **Area:** auth/apikeys · **Est:** M · **Deps:** P1-037 · **Prio:** P1
  - **Spec:** Jobs record key_id; usage endpoints filter by key; admin view shows prefix.
  - **AC:**
    - [ ] attribution verified

- [ ] **P1-039** — API-key suite
  - **Type:** test · **Area:** auth/apikeys · **Est:** M · **Deps:** P1-038 · **Prio:** P1
  - **Spec:** Lifecycle: create, use, rotate, old-valid-in-window, expired, revoked; scope enforcement (jobs:read cannot create).
  - **AC:**
    - [ ] suite green

- [ ] **P1-040** — Key ACL matrix rows
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-039 · **Prio:** P1
  - **Spec:** Matrix rows for api-key on all /v1 routes (no admin ever, no billing); checked by matrix test registry.
  - **AC:**
    - [ ] matrix complete

- [ ] **P1-041** — API-key docs page
  - **Type:** docs · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-040 · **Prio:** P1
  - **Spec:** Getting-started curl/TS/py examples per api-spec; security notes (storage, rotation).
  - **AC:**
    - [ ] docs committed

---

## E1.5 — RBAC & authz (area: auth/rbac)

- [ ] **P1-042** — Roles + permission matrix
  - **Type:** feat · **Area:** auth/rbac · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** user/support/admin capability table (code constants), role-guard middleware, 403 problem+json; matrix is the single source consumed by tests.
  - **AC:**
    - [ ] matrix test registry live

- [ ] **P1-043** — Admin route guard
  - **Type:** feat · **Area:** auth/rbac · **Est:** M · **Deps:** P1-042 · **Prio:** P1
  - **Spec:** All /v1/admin/* behind admin role + audit middleware (actor, action, target, before/after).
  - **AC:**
    - [ ] guard + audit verified

- [ ] **P1-044** — Support read-only
  - **Type:** feat · **Area:** auth/rbac · **Est:** M · **Deps:** P1-043 · **Prio:** P1
  - **Spec:** Support can read user/job/admin views only; writes 403; matrix asserts.
  - **AC:**
    - [ ] write attempts 403

- [ ] **P1-045** — IDOR sweep v1
  - **Type:** test · **Area:** auth/rbac · **Est:** M · **Deps:** P1-044 · **Prio:** P1
  - **Spec:** For each owned resource (jobs, uploads, keys, sessions, webhooks): cross-user access returns 404 (not 200/403 leak) per test plan.
  - **AC:**
    - [ ] sweep green

- [ ] **P1-046** — Privileged action audit middleware
  - **Type:** feat · **Area:** auth/rbac · **Est:** S · **Deps:** P1-045 · **Prio:** P1
  - **Spec:** Route metadata drives automatic audit for admin routes (no forgotten manual calls); rows asserted.
  - **AC:**
    - [ ] middleware audits all admin writes

- [ ] **P1-047** — Authz docs
  - **Type:** docs · **Area:** auth/rbac · **Est:** S · **Deps:** P1-046 · **Prio:** P1
  - **Spec:** Matrix rendered into admin guide; change procedure (matrix + tests together).
  - **AC:**
    - [ ] docs committed

- [ ] **P1-048** — Role escalation tests
  - **Type:** test · **Area:** auth/rbac · **Est:** S · **Deps:** P1-047 · **Prio:** P1
  - **Spec:** Attempts: user to admin endpoints, support to writes, api-key to admin: all 403 + audited; token role tamper rejected (signature).
  - **AC:**
    - [ ] all attempts blocked + audited

---

## E1.6 — Plans & entitlements (area: billing/plans)

- [ ] **P1-049** — Plans service
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Catalog from db (seeded) + cache; entitlement struct {daily_images, daily_video_seconds, max_bytes, result_ttl_hours, concurrency, lane, features}; effective-plan resolution (subscription override).
  - **AC:**
    - [ ] resolution tested

- [ ] **P1-050** — Quota counters
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** P1-049 · **Prio:** P1
  - **Spec:** Redis atomic daily counters (UTC reset) + usage_counters write-through; video-seconds estimator at job create; nightly reconciliation fixes drift.
  - **AC:**
    - [ ] UTC boundary clock-injected test

- [ ] **P1-051** — Quota middleware
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** P1-050 · **Prio:** P1
  - **Spec:** Pre-check on job create (images + video seconds + concurrency slot); 429 problem+json with upgrade hint + PF-3001/3002; X-Quota-* headers.
  - **AC:**
    - [ ] rejection shape tested
    - [ ] headers present

- [ ] **P1-052** — Quota headers everywhere
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-051 · **Prio:** P1
  - **Spec:** Job-creating routes attach remaining quota; snapshot in /v1/me; UI contract documented.
  - **AC:**
    - [ ] headers on all create routes

- [ ] **P1-053** — Concurrency slots
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-052 · **Prio:** P1
  - **Spec:** Per-user running-jobs cap per plan; crash-safe slot acquire/release with TTL; slot-leak recovery tested.
  - **AC:**
    - [ ] leak recovery green

- [ ] **P1-054** — Overage policy
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-053 · **Prio:** P1
  - **Spec:** Reject with clear message + plan matrix link; no silent queueing; admin quota-grant bypass audited (E1.13).
  - **AC:**
    - [ ] no silent overage

- [ ] **P1-055** — Plan feature flags
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-054 · **Prio:** P1
  - **Spec:** features jsonb (api_keys, webhooks, batch, priority_lane) checked by services; 403 PF-3003 with upgrade hint.
  - **AC:**
    - [ ] gated features tested

- [ ] **P1-056** — Admin plan editor
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** P1-055 · **Prio:** P1
  - **Spec:** GET/PUT /v1/admin/plans (validated entitlements + stripe price mapping), cache invalidation, audited; plan removal requires no-active-subs check.
  - **AC:**
    - [ ] editor + guard tests

- [ ] **P1-057** — Entitlement suite
  - **Type:** test · **Area:** billing/plans · **Est:** M · **Deps:** P1-056 · **Prio:** P1
  - **Spec:** Free vs Pro behaviors: limits trip, UTC reset, concurrency slots, TTL differences; reconciliation catches drift.
  - **AC:**
    - [ ] suite green

- [ ] **P1-058** — Pricing config docs
  - **Type:** docs · **Area:** billing/plans · **Est:** S · **Deps:** P1-057 · **Prio:** P1
  - **Spec:** Where defaults live (open-questions #2), safe change procedure (config + stripe dashboard + seed), rollout notes.
  - **AC:**
    - [ ] docs committed

---

## E1.7 — Stripe billing (area: billing/stripe)

- [ ] **P1-059** — BillingProvider trait
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Trait: create_checkout, create_portal, fetch_subscription, map_webhook(event) to DomainEvent, customer ops; typed error; rationale documented (FR-P-05).
  - **AC:**
    - [ ] trait + docs committed

- [ ] **P1-060** — Stripe adapter
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-059 · **Prio:** P1
  - **Spec:** async-stripe: checkout session (price from plan config), portal session, subscription fetch; test-mode enforced (live keys rejected without owner flag).
  - **AC:**
    - [ ] live-key guard tested

- [ ] **P1-061** — Products/prices sync job
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-060 · **Prio:** P1
  - **Spec:** Nightly sync plan catalog to Stripe products/prices (test mode) with drift report; manual trigger admin endpoint.
  - **AC:**
    - [ ] drift report produced

- [ ] **P1-062** — Checkout endpoint
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-061 · **Prio:** P1
  - **Spec:** POST /v1/billing/checkout: auth, plan from body, customer reuse, success/cancel URLs from config; idempotency honored.
  - **AC:**
    - [ ] idempotent checkout

- [ ] **P1-063** — Webhook receiver
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-062 · **Prio:** P1
  - **Spec:** POST /v1/webhooks/stripe: constant-time signature verify, raw-body capture, event table insert (unique event id) for idempotency, then dispatch; 4xx only on bad signature.
  - **AC:**
    - [ ] tamper rejected
    - [ ] replay no-op

- [ ] **P1-064** — Subscription state machine
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-063 · **Prio:** P1
  - **Spec:** trialing/active/past_due/canceled transitions; side effects: entitlement cache flush, emails (welcome, dunning, canceled), audit.
  - **AC:**
    - [ ] invalid transitions rejected

- [ ] **P1-065** — Webhook event handlers
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-064 · **Prio:** P1
  - **Spec:** checkout.session.completed, invoice.paid/payment_failed, customer.subscription.updated/deleted mapped; unknown event logged + 200 (no retry storm).
  - **AC:**
    - [ ] mapping table tested

- [ ] **P1-066** — Idempotency + replay safety
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-065 · **Prio:** P1
  - **Spec:** Replayed event: single effect (event-table unique); out-of-order events guarded by state machine; reordered-delivery tests.
  - **AC:**
    - [ ] replay/order tests green

- [ ] **P1-067** — Billing portal endpoint
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-066 · **Prio:** P1
  - **Spec:** POST /v1/billing/portal returns portal URL (test mode); portal configuration note (invoice display, cancel behavior).
  - **AC:**
    - [ ] portal URL returned

- [ ] **P1-068** — Invoices list
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-067 · **Prio:** P1
  - **Spec:** GET /v1/billing/invoices from stripe (cached 5m) mapped to our shape; stripe-hosted download URLs.
  - **AC:**
    - [ ] shape contract tested

- [ ] **P1-069** — Upgrade/downgrade proration
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-068 · **Prio:** P1
  - **Spec:** Upgrade via checkout with immediate proration; downgrade schedules at period end; entitlement switch timing documented + tested.
  - **AC:**
    - [ ] timing verified

- [ ] **P1-070** — Dunning emails
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-069 · **Prio:** P1
  - **Spec:** past_due sequence (day 0/3/7), retry window, suspend-at-day-14 policy job (configurable), audited.
  - **AC:**
    - [ ] sequence simulated via test-clock

- [ ] **P1-071** — Refund/cancel policy endpoints
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-070 · **Prio:** P1
  - **Spec:** Support-facing: immediate cancel (no proration) + refund guidance (Stripe dashboard) - audited note; ToS policy snippet.
  - **AC:**
    - [ ] flow audited

- [ ] **P1-072** — Stripe suite
  - **Type:** test · **Area:** billing/stripe · **Est:** M · **Deps:** P1-071 · **Prio:** P1
  - **Spec:** Test-clock lifecycle (trial to active to past_due to canceled), signature tamper, replay, idempotency, proration math units.
  - **AC:**
    - [ ] suite green

---

## E1.8 — Razorpay adapter (area: billing/razorpay)

- [ ] **P1-073** — Razorpay adapter
  - **Type:** feat · **Area:** billing/razorpay · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** BillingProvider impl: checkout link, subscription mapping (best-effort to our states), PF_RAZORPAY_* config; flag OFF default; India-market intent documented.
  - **AC:**
    - [ ] flag-off endpoints 404

- [ ] **P1-074** — Razorpay webhook verify
  - **Type:** feat · **Area:** billing/razorpay · **Est:** M · **Deps:** P1-073 · **Prio:** P1
  - **Spec:** HMAC sha256 (x-razorpay-signature), event mapping to DomainEvents, idempotent via event table.
  - **AC:**
    - [ ] tamper rejected
    - [ ] idempotent

- [ ] **P1-075** — Razorpay adapter tests
  - **Type:** test · **Area:** billing/razorpay · **Est:** M · **Deps:** P1-074 · **Prio:** P1
  - **Spec:** Mock server: signature pass/fail, mapping table, idempotency; flag-off behavior tested.
  - **AC:**
    - [ ] mock suite green

- [ ] **P1-076** — GST/invoice note
  - **Type:** feat · **Area:** billing/razorpay · **Est:** S · **Deps:** P1-075 · **Prio:** P1
  - **Spec:** Config fields for GSTIN passthrough (provider metadata), ToS tax clause stub, docs.
  - **AC:**
    - [ ] fields flow through

- [ ] **P1-077** — Provider parity check
  - **Type:** feat · **Area:** billing/razorpay · **Est:** S · **Deps:** P1-076 · **Prio:** P1
  - **Spec:** Parity doc: what works vs Stripe, what is stubbed; ADR for launch sequencing.
  - **AC:**
    - [ ] parity doc committed

- [ ] **P1-078** — Flag enable runbook
  - **Type:** chore · **Area:** billing/razorpay · **Est:** S · **Deps:** P1-077 · **Prio:** P1
  - **Spec:** Steps + risks to enable razorpay in prod; owner checklist in DECISIONS.md template.
  - **AC:**
    - [ ] checklist committed

---

## E1.9 — Usage metering (area: billing/metering)

- [ ] **P1-079** — Usage counters service
  - **Type:** feat · **Area:** billing/metering · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Write-through usage_counters (user, day) + redis hot counters; UTC day boundary; video-seconds accumulate on completion (estimate at create, true-up later).
  - **AC:**
    - [ ] true-up math tested

- [ ] **P1-080** — Usage API
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-079 · **Prio:** P1
  - **Spec:** GET /v1/usage: today + 30-day buckets; per-key breakdown when api key used; shape documented.
  - **AC:**
    - [ ] shape contract tested

- [ ] **P1-081** — Admin usage view API
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-080 · **Prio:** P1
  - **Spec:** Per-user 30d + totals; CSV export endpoint (admin) for billing disputes.
  - **AC:**
    - [ ] export parses

- [ ] **P1-082** — Reconciliation job
  - **Type:** feat · **Area:** billing/metering · **Est:** M · **Deps:** P1-081 · **Prio:** P1
  - **Spec:** Nightly: redis vs db drift, db wins (source of truth), anomaly report; audit on corrections beyond threshold.
  - **AC:**
    - [ ] drift corrected + audited

- [ ] **P1-083** — Abuse spike detection
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-082 · **Prio:** P1
  - **Spec:** Velocity rules (N jobs/minute threshold) create abuse_reports entries + alert; auto-suspend flag OFF default.
  - **AC:**
    - [ ] spike detected
    - [ ] flag off

- [ ] **P1-084** — Metering suite
  - **Type:** test · **Area:** billing/metering · **Est:** M · **Deps:** P1-083 · **Prio:** P1
  - **Spec:** Races (parallel creates under limit), midnight rollover (clock), true-up math, drift reconciliation.
  - **AC:**
    - [ ] suite green

- [ ] **P1-085** — Quota warning emails
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-084 · **Prio:** P1
  - **Spec:** 80%/100% thresholds (config) warning emails with once/day cap; outbox tests cover.
  - **AC:**
    - [ ] capped correctly

- [ ] **P1-086** — Metering design docs
  - **Type:** docs · **Area:** billing/metering · **Est:** S · **Deps:** P1-085 · **Prio:** P1
  - **Spec:** Counter semantics, edge cases (clock skew, retry double-count guard), reconciliation policy.
  - **AC:**
    - [ ] docs committed

---

## E1.10 — Audit & abuse (area: audit)

- [ ] **P1-087** — Audit service
  - **Type:** feat · **Area:** audit · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Append-only writes via service only (no direct repo writes elsewhere); actor/action/target/before/after/reason; db grant test proves UPDATE/DELETE denied.
  - **AC:**
    - [ ] append-only enforced

- [ ] **P1-088** — Audit query API
  - **Type:** feat · **Area:** audit · **Est:** S · **Deps:** P1-087 · **Prio:** P1
  - **Spec:** GET /v1/admin/audit?actor=&action=&target=&from=&to= cursor-paginated; indexes per data-model.
  - **AC:**
    - [ ] filters + pagination tested

- [ ] **P1-089** — Abuse intake
  - **Type:** feat · **Area:** audit · **Est:** M · **Deps:** P1-088 · **Prio:** P1
  - **Spec:** POST /v1/abuse (public, rate-limited, honeypot field): job id/reporter contact/reason creates row; ack email template.
  - **AC:**
    - [ ] honeypot drops bots

- [ ] **P1-090** — Abuse queue + actions
  - **Type:** feat · **Area:** audit · **Est:** M · **Deps:** P1-089 · **Prio:** P1
  - **Spec:** Admin queue (open/reviewing/resolved) with actions: reject job, suspend user, dismiss - all audited; SLA timestamps (72h target).
  - **AC:**
    - [ ] actions audited

- [ ] **P1-091** — Enforcement workflow
  - **Type:** feat · **Area:** audit · **Est:** S · **Deps:** P1-090 · **Prio:** P1
  - **Spec:** Warning to suspension to termination ladder helpers + template emails; suspension effects (sessions killed, jobs held) tested.
  - **AC:**
    - [ ] ladder e2e green

- [ ] **P1-092** — Audit/abuse suite
  - **Type:** test · **Area:** audit · **Est:** M · **Deps:** P1-091 · **Prio:** P1
  - **Spec:** Matrix-driven: every privileged action audited; abuse lifecycle; retention prunes per NFR-40.
  - **AC:**
    - [ ] suite green

- [ ] **P1-093** — Abuse ops playbook
  - **Type:** docs · **Area:** audit · **Est:** S · **Deps:** P1-092 · **Prio:** P1
  - **Spec:** Triage checklist, evidence preservation (artifacts within TTL), DMCA-style takedown response stub.
  - **AC:**
    - [ ] playbook committed

---

## E1.11 — Onboarding & consent (area: onboarding)

- [ ] **P1-094** — Attestation capture
  - **Type:** feat · **Area:** onboarding · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Attestations table + service: required at upload-create and referenced at job-create; text versioned; ip_hash; client web/api-key; immutable rows.
  - **AC:**
    - [ ] job without attestation rejected (PF-4001)

- [ ] **P1-095** — Consent versioning
  - **Type:** feat · **Area:** onboarding · **Est:** M · **Deps:** P1-094 · **Prio:** P1
  - **Spec:** tos_version + aup_version on users; accept at register; bump flow re-accept banner (config-driven); audited.
  - **AC:**
    - [ ] bump forces re-accept

- [ ] **P1-096** — Onboarding checklist API
  - **Type:** feat · **Area:** onboarding · **Est:** S · **Deps:** P1-095 · **Prio:** P1
  - **Spec:** GET /v1/me/onboarding: steps (verify email, accept ToS, first upload, first job, optional api key); consumed by web; completion events.
  - **AC:**
    - [ ] steps transition correctly

- [ ] **P1-097** — Welcome email
  - **Type:** feat · **Area:** onboarding · **Est:** S · **Deps:** P1-096 · **Prio:** P1
  - **Spec:** Template + send on verification; quickstart link + honest quality-promise one-liner.
  - **AC:**
    - [ ] copy passes honesty rule

- [ ] **P1-098** — Attestation admin review
  - **Type:** feat · **Area:** onboarding · **Est:** M · **Deps:** P1-097 · **Prio:** P1
  - **Spec:** Admin list + flag (velocity anomalies) + link to abuse queue; read-model only v1.
  - **AC:**
    - [ ] anomalies flagged

- [ ] **P1-099** — Consent/attestation tests
  - **Type:** test · **Area:** onboarding · **Est:** M · **Deps:** P1-098 · **Prio:** P1
  - **Spec:** Job without attestation impossible; version bump forces re-accept; immutability enforced.
  - **AC:**
    - [ ] suite green

- [ ] **P1-100** — Onboarding spec
  - **Type:** docs · **Area:** onboarding · **Est:** S · **Deps:** P1-099 · **Prio:** P1
  - **Spec:** UI flow contract for Phase 4 (screens, states), copy points, drop-off metric events list.
  - **AC:**
    - [ ] spec committed

---

## E1.12 — Email polish (area: mailer)

- [ ] **P1-101** — Weekly digest email
  - **Type:** feat · **Area:** mailer · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Digest template (usage, plan, tips) behind flag OFF default; opt-out in user settings.
  - **AC:**
    - [ ] flag respected

- [ ] **P1-102** — Quota/billing email set
  - **Type:** feat · **Area:** mailer · **Est:** S · **Deps:** P1-101 · **Prio:** P1
  - **Spec:** quota-80, quota-100, payment-failed, subscription-activated templates wired to events.
  - **AC:**
    - [ ] triggers verified

- [ ] **P1-103** — Unsubscribe/preferences
  - **Type:** feat · **Area:** mailer · **Est:** S · **Deps:** P1-102 · **Prio:** P1
  - **Spec:** Settings jsonb prefs; transactional exempt; List-Unsubscribe header for future marketing.
  - **AC:**
    - [ ] prefs honored

- [ ] **P1-104** — Email regression suite
  - **Type:** test · **Area:** mailer · **Est:** S · **Deps:** P1-103 · **Prio:** P1
  - **Spec:** All templates snapshot-tested; link tokens single-use; no PII in subject lines.
  - **AC:**
    - [ ] suite green

- [ ] **P1-105** — Email matrix docs
  - **Type:** docs · **Area:** mailer · **Est:** S · **Deps:** P1-104 · **Prio:** P1
  - **Spec:** Table: trigger, template, cap, audience; ops notes.
  - **AC:**
    - [ ] matrix committed

---

## E1.13 — Support tooling (area: support)

- [ ] **P1-106** — Support user view
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Read-only user detail endpoint (support role): profile, plan, usage, recent jobs, session meta; no secrets.
  - **AC:**
    - [ ] role-gated + no secrets

- [ ] **P1-107** — Manual quota grant
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-106 · **Prio:** P1
  - **Spec:** POST /v1/admin/users/{id}/quota-grant {images?, video_seconds?, ttl_hours, reason}: audited, expiring, honored by quota middleware.
  - **AC:**
    - [ ] expiry honored

- [ ] **P1-108** — Refund tool stub
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-107 · **Prio:** P1
  - **Spec:** Guided checklist endpoint returning Stripe dashboard deep-link + required fields; actual refund stays in Stripe UI; audited note.
  - **AC:**
    - [ ] checklist complete

- [ ] **P1-109** — User lookup helper
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-108 · **Prio:** P1
  - **Spec:** Resolve by email/id/job-id to user (support role); audited access log for privacy.
  - **AC:**
    - [ ] lookups audited

- [ ] **P1-110** — Ticket notes
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-109 · **Prio:** P1
  - **Spec:** Admin-only append-only notes on users (jsonb list), audit-tracked; support flow uses them.
  - **AC:**
    - [ ] append-only + audited

- [ ] **P1-111** — Support suite
  - **Type:** test · **Area:** support · **Est:** S · **Deps:** P1-110 · **Prio:** P1
  - **Spec:** Role enforcement, audit rows, quota-grant expiry honored.
  - **AC:**
    - [ ] suite green
