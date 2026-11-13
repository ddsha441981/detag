# Security Testing Plan

Executed as tickets in Phase 1 (authz), Phase 2/3 (upload abuse), Phase 5 (full sweep). Each item = a named test that lives in CI, not a one-off manual check.

Threat model: `docs/threat-model.md` — the living threat model this plan tests against. If a test here covers a threat, the threat's ID (T1–T17) is the cross-reference.

## Authorization matrix sweep (IDOR)

For every `/v1` endpoint × role {anonymous, user-A, user-B, support, admin, api-key-scoped}: assert allowed matrix. Auto-generated table test in `apps/api/tests/authz_matrix.rs`; adding an endpoint without a matrix row fails the test (registry pattern: routes self-register their matrix entry).

## Session & token tests

- Refresh rotation: reuse of rotated refresh ⇒ whole family revoked (PF-1007), event logged.
- JWT: alg-confusion attempt (alg:none, HS/RS swap) rejected; exp/nbf enforced; kid confusion rejected.
- Password flows: reset token single-use; old password invalid after reset; email-verification enforced before jobs.
- Cookie flags: Secure, HttpOnly, SameSite=Lax asserted in integration tests.
- Login (P1-003, T17): unknown email and wrong password answer byte-identical 401 `PF-1001` (the test compares the two bodies — no enumeration); constant-time verify via the precomputed dummy hash (an unknown email costs one Argon2id verify, like a wrong password); per-account bucket (10 burst / 1 per min) trips pre-auth — a correct-password attempt after the limit is 429 `PF-3007` with `Retry-After`; per-IP bucket trips on login too (the middleware's bucket, proven with a per-test XFF address).
- Lockout (P1-004, T17): five failures lock the account AND the IP — the next login (even with the correct password) is 423 `PF-1008` with `Retry-After` ≈ 60 s (the first backoff step; the full curve 60 → 120 → … → 3600 is proven exactly by the unit test in `crates/core/src/lockout.rs`), and a further attempt while locked does not extend the lock; a success resets both counters (four failures + success + five failures locks, never earlier); the lockout mail is queued exactly once per episode (`lockout-notice`, `login.locked` audited); five failures from one IP for different emails lock the IP — a fresh account from that IP is 423; twenty concurrent failures never lose an increment (counter ends at exactly 20, lock at the 1-hour cap); registering an email clears a counter the address accumulated before it existed.
- Profile + auth extractor (P1-005): `GET`/`PATCH /v1/me` without a credential, with a malformed JWT, or with a valid JWT for an unknown user is 401 `PF-1001`; an expired JWT is 401 `PF-1011` (the extractor's own unit test proves the three no-DB rejections; the integration tests prove the happy path and the unknown-user case through the real router). A Bearer API key or a session cookie is 401 `PF-1001` today (no key table read yet; the cookie leg is P1-016). The profile never leaks the password hash (asserted on the GET body); the email-change intent is duplicate-checked (409 `PF-2011`) and the `profile.update` audit carries the pending email as sha256 only.
- Email change (P1-006, T18): setting a pending email mints an `email_change` token + queues the verification mail to the new address and the takeover alert to the old address (the alert carries no token and never names the new address — asserted on the outbox rows); confirming swaps the email atomically (consume + swap + audit in one tx) and verifies the new address; a registration token at confirm is 401 `PF-1010` (wrong purpose, not consumed — asserted: the token still verifies afterwards); an expired change token is 401 `PF-1011` (not consumed); a cleared intent deletes the change token (confirm then 401s); an address taken meanwhile is 409 `PF-2011` with the token surviving (tx rolled back) and the intent still pending; a resend-verification kills a live change token (one live token per user — the pending intent survives and is re-triggered by clearing + re-setting).

## Upload abuse suite

| Attack | Expected |
| --- | --- |
| Zip bomb inside batch ZIP | rejected PF-2006, resource caps on extraction |
| PNG decompression bomb (huge logical dims) | dimension/byte budget rejected at sniff (FR-U-04) |
| Polyglot (JPEG+HTML/JS) | magic-byte sniff + re-decode verify rejects |
| SVG | rejected outright (PF-2001) |
| EXIF exploit strings (oversized tags) | parser hardened; fuzz nightly (cargo-fuzz on EXIF/metadata parsers) |
| Mismatched extension vs magic | magic wins (FR-U-03) |
| Path traversal in filename | sanitized to opaque storage key (ADR-0011) |

## Security smoke suite (P0-107)

`apps/api/tests/security_smoke.rs` — one named gate over the
five Phase 0 hardening ACs, driven through the real router.
Run it alone with `just security-smoke`; `just verify` runs it
as part of the workspace suite. It deliberately duplicates a
few assertions from the deeper suites — a smoke suite that
only re-runs other suites proves nothing; it owns its five
checks:

| Check | Threat | What it asserts |
| --- | --- | --- |
| Headers present | T7/T8 | The secure-header table verbatim + request id + ACAO on a real response; no HSTS in dev |
| Rate limit trips at threshold | T11 | Burst 5 admits 5, the 6th is 429 PF-3007 with `Retry-After` |
| Oversized body rejected | T5 | 65 KiB on the 64 KiB auth class is 413 PF-2003; the same body on jobs (1 MiB) is admitted |
| Method-not-allowed | — | DELETE on GET-only `/healthz` is 405 with `Allow: GET`; the body is axum's bare 405, not problem+json — the mapper rewrites 400/404/413/415/422 only, and whether 405 belongs in the envelope is a Phase 1 question |
| No swagger in prod profile | T9 | `/docs` and `/docs/` are 404 (PF-2010) with `PF_ENVIRONMENT=prod`; `/healthz` still answers 200 with HSTS; dev `/docs` 303-redirects to `/docs/` |

## API & webhook hardening

- Body caps per route class (P0-104): auth/meta 64 KiB, jobs/admin 1 MiB, billing (webhooks) 256 KiB; over-cap is 413 PF-2003, tested per class. Uploads are exempt (bytes never transit the api).
- Dimension caps (P0-104, FR-U-04): 8000x8000 per side, 64 MP total, enforced at job-create by `validate_dimensions` (PF-2005, 413, fail-closed on missing dimensions); the sniff-time dimension check in the abuse table above lands with intake (Phase 2/3).
- Webhook SSRF: endpoint URL must resolve to public HTTPS; private/link-local CIDRs denied; redirect-following disabled; tested with a listener on 169.254.169.254-style address.
- Outbound webhook HMAC: constant-time compare; replay window enforced; tampered payload rejected.
- Idempotency: replayed Idempotency-Key returns original response; concurrent duplicates create one job (integration test with racing requests).
- Rate limits: per-IP + per-route token buckets enforced by the middleware (P0-103; race-safe — no over-admission under concurrency, proven by test); per-account + per-key tiers land with Phase 1 auth; lockout backoff curve asserted.
- Injection: sqlx parameterized everywhere (CI grep forbids `format!`-built SQL); no string-built shell calls (ffmpeg args as argv array, never shell string).

## Headers & transport

CSP (web), HSTS (prod only — dev is plain http, so dev responses carry the rest of the table but no HSTS), X-Content-Type-Options, Referrer-Policy, frame-ancestors none for app routes; the Swagger UI at `/docs` gets its own same-origin CSP (the api's `default-src 'none'` would block its JS) — asserted by a route test against a header table. TLS terminated at caddy in prod; internal services never exposed (compose networks: `internal` vs `edge`).

## Secrets & supply chain

gitleaks CI; `cargo deny` advisories+licenses gate; Dependabot-style weekly bump window (Phase 5 cadence); no secrets in fixtures, logs, or error messages (test asserts error bodies contain no stack traces in prod profile).

## Reporting

Findings land in `09-STATE/ISSUES.md` with severity per `06-DELIVERY/01-bug-triage.md`. S1 security = stop feature work immediately. A security summary table (test → result) ships in the Phase 5 release checklist.

Phase 0 baseline: `docs/05-QUALITY/05-security-baseline.md` (P0-108) — what is enforced where (CI + runtime + ops) and the checklist mapping of every section above, with the known gaps stated plainly. Update it when a section's status changes.
